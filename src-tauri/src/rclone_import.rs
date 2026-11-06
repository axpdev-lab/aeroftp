// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from rclone configuration files.
//!
//! Parses `rclone.conf` (INI format), maps rclone backend types to AeroFTP
//! ProviderType, and de-obfuscates rclone "obscured" passwords (AES-256-CTR
//! with a well-known key: NOT real encryption).
//!
//! Imported credentials are stored in our AES-256-GCM vault, upgrading security
//! from rclone's reversible obfuscation to proper authenticated encryption.

// Public: it is the element type of `RcloneImportResult::servers`.
pub use crate::profile_export::ServerProfileExport;
use crate::util::endpoint_stays_on_this_machine;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ============ rclone obscure: AES-256-CTR with published key ============
// Source: https://github.com/rclone/rclone/blob/master/fs/config/obscure/obscure.go
// This is NOT encryption: the key is public. We reveal it to store in our vault.

const RCLONE_CRYPT_KEY: [u8; 32] = [
    0x9c, 0x93, 0x5b, 0x48, 0x73, 0x0a, 0x55, 0x4d, 0x6b, 0xfd, 0x7c, 0x63, 0xc8, 0x86, 0xa9, 0x2b,
    0xd3, 0x90, 0x19, 0x8e, 0xb8, 0x12, 0x8a, 0xfb, 0xf4, 0xde, 0x16, 0x2b, 0x8b, 0x95, 0xf6, 0x38,
];

/// AES-256-CTR with rclone's published key over `IV (16 bytes) || ciphertext`,
/// the bytes an obscured value decodes to.
fn decrypt_obscured(bytes: &[u8]) -> Result<Vec<u8>, String> {
    use aes::cipher::{KeyIvInit, StreamCipher};

    if bytes.len() < 16 {
        return Err("obscured value too short (need at least 16-byte IV)".into());
    }
    let iv = &bytes[..16];
    let mut buf = bytes[16..].to_vec();

    type Aes256Ctr = ctr::Ctr128BE<aes::Aes256>;
    let mut cipher = Aes256Ctr::new((&RCLONE_CRYPT_KEY).into(), iv.into());
    cipher.apply_keystream(&mut buf);
    Ok(buf)
}

/// Reveal an rclone-obscured value: base64url(IV_16bytes || AES-256-CTR(plaintext)).
///
/// rclone only ever writes raw URL-safe base64 (`obscure.Obscure`, v1.75.1).
/// The retry in the standard alphabet is not a form rclone produces: it is
/// AeroFTP's own leniency, kept for `rclone_crypt`, the caller that still
/// accepts a value typed by hand. The importer reads `IsPassword` fields with
/// [`reveal_rclone_password`], which does not retry.
pub(crate) fn reveal_obscured(obscured: &str) -> Result<String, String> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    let ciphertext = URL_SAFE_NO_PAD
        .decode(obscured)
        .or_else(|_| {
            use base64::engine::general_purpose::STANDARD;
            STANDARD.decode(obscured)
        })
        .map_err(|e| format!("base64 decode: {}", e))?;
    String::from_utf8(decrypt_obscured(&ciphertext)?)
        .map_err(|e| format!("UTF-8 decode after reveal: {}", e))
}

/// Reveal an rclone `IsPassword` value the way rclone's `obscure.Reveal` does
/// (v1.75.1): raw URL-safe base64 without padding, with the discarded low bits
/// of the last symbol ignored as Go's `RawURLEncoding` ignores them, no retry
/// in the standard alphabet and no plaintext fallback. One difference is left:
/// rclone returns the decrypted bytes as they are, while this requires UTF-8,
/// since the secret becomes a `String`; a value rclone obscured from typed text
/// always is. rclone only ever writes these fields obscured and cannot use one
/// that does not reveal, so a failure here means the remote carries no usable
/// password, not that the value is the password.
pub(crate) fn reveal_rclone_password(value: &str) -> Result<String, String> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    use base64::Engine;

    const RCLONE_REVEAL_BASE64: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::RequireNone),
    );
    // The decoder's own error names the offending character and its offset,
    // which for a plaintext password is a piece of the password: keep it out
    // of a message that reaches the log and the import report.
    let bytes = RCLONE_REVEAL_BASE64
        .decode(value)
        .map_err(|_| "not raw URL-safe base64".to_string())?;
    String::from_utf8(decrypt_obscured(&bytes)?)
        .map_err(|_| "it reveals to bytes that are not UTF-8".to_string())
}

/// [`reveal_rclone_password`] for a value exactly as rclone writes one: raw
/// URL-safe base64 whose discarded low bits are zero, as `obscure.Obscure`
/// always leaves them. A probe for "could rclone have written this?": the
/// lenient decoder also accepts values rclone never produces (about one
/// 26-character string in twenty), which a probe must not count.
pub(crate) fn reveal_canonical_rclone(value: &str) -> Result<String, String> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| "not canonical raw URL-safe base64".to_string())?;
    String::from_utf8(decrypt_obscured(&bytes)?)
        .map_err(|_| "it reveals to bytes that are not UTF-8".to_string())
}

/// What an rclone `IsPassword` field of a remote holds.
enum RcloneSecretField {
    Absent,
    Revealed(String),
    /// Obscured, and revealing to nothing: `rclone obscure ""`.
    Empty,
    /// Present, but not a value rclone could reveal; the reason, which never
    /// quotes the value.
    Unreadable(String),
}

/// Reads an rclone `IsPassword` field of `remote` the way rclone does
/// ([`reveal_rclone_password`]).
fn rclone_password_field(remote: &RcloneRemote, key: &str) -> RcloneSecretField {
    let Some(value) = remote.get(key).map(|v| v.trim()).filter(|v| !v.is_empty()) else {
        return RcloneSecretField::Absent;
    };
    match reveal_rclone_password(value) {
        Ok(revealed) if revealed.is_empty() => RcloneSecretField::Empty,
        Ok(revealed) => RcloneSecretField::Revealed(revealed),
        Err(e) => RcloneSecretField::Unreadable(e),
    }
}

/// What the header of an rclone.conf written by AeroFTP's own export says
/// about it. Every AeroFTP export since the first one (v3.4.7) opens with
/// `# Generated by AeroFTP` and `# Exported: <RFC 3339>`. Files that name the
/// AeroFTP version put it on the first line (`# Generated by AeroFTP 4.2.0 - ...`).
/// rclone keeps both lines when it rewrites the file. A build from before the
/// version bump still writes the version it was compiled with, so the line is
/// read from whatever version it names, not from a release number in this comment.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct AeroftpExport {
    /// `None` when the stamp is missing or does not parse.
    exported: Option<chrono::DateTime<chrono::Utc>>,
    /// `None` when the header does not name an AeroFTP version.
    version: Option<(u32, u32, u32)>,
}

/// `major.minor.patch` of an AeroFTP version, ignoring a pre-release suffix.
fn parse_aeroftp_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split(['-', '+']).next()?.split('.');
    let mut next = || parts.next()?.parse::<u32>().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

/// The AeroFTP export header of `content`, `None` for any other rclone.conf.
fn aeroftp_export_header(content: &str) -> Option<AeroftpExport> {
    let mut export: Option<AeroftpExport> = None;
    for line in content.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('#') {
            break;
        }
        if let Some(rest) = line.strip_prefix("# Generated by AeroFTP") {
            let version = rest
                .split_whitespace()
                .next()
                .and_then(parse_aeroftp_version);
            export = Some(AeroftpExport {
                exported: None,
                version,
            });
        } else if let Some(stamp) = line.strip_prefix("# Exported:") {
            if let Some(export) = export.as_mut() {
                export.exported = chrono::DateTime::parse_from_rfc3339(stamp.trim())
                    .ok()
                    .map(|t| t.with_timezone(&chrono::Utc));
            }
        }
    }
    export
}

/// Until when AeroFTP's own export wrote a field obscured that rclone keeps
/// plain, as the release of the version that fixed it: S3 `secret_access_key`
/// and the Azure `key` until v3.7.6 (bfa25d257), the Swift `key` until v4.0.5
/// (9abfe2634). A file stamped before that release came from a version that
/// obscured the field.
const AEROFTP_OBSCURED_S3_AZURE_UNTIL: (&str, &str) = ("2026-05-08T19:10:06Z", "3.7.6");
const AEROFTP_OBSCURED_SWIFT_UNTIL: (&str, &str) = ("2026-06-16T18:48:54Z", "4.0.5");

/// Shortest revealed plaintext accepted as a key AeroFTP used to obscure.
/// A clear key of 23 to 31 characters decodes to 1 to 7 bytes, and those
/// bytes are printable ASCII often enough (about 37% at one byte) to be
/// stored as the secret. Eight characters is the shortest real secret this
/// path is meant to recover (a Swift password); S3, Azure and B2 keys are longer.
const MIN_REVEALED_PLAIN_SECRET_LEN: usize = 8;

/// Appends the notes about credentials left out to the warning the profile
/// already carries.
fn with_credential_notes(mut mapped: MappedProfile, notes: Vec<String>) -> MappedProfile {
    if !notes.is_empty() {
        let notes = notes.join("; ");
        mapped.credential_warning = Some(match mapped.credential_warning.take() {
            Some(previous) => format!("{previous}; {notes}"),
            None => notes,
        });
    }
    mapped
}

/// Obscure a plaintext password using rclone's AES-256-CTR scheme.
/// Output: base64url(random_IV_16 || AES-256-CTR(plaintext))
pub(crate) fn obscure_password(plaintext: &str) -> Result<String, String> {
    use aes::cipher::{KeyIvInit, StreamCipher};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    let iv = crate::crypto::random_bytes(16);
    let mut buf = plaintext.as_bytes().to_vec();

    type Aes256Ctr = ctr::Ctr128BE<aes::Aes256>;
    let mut cipher = Aes256Ctr::new((&RCLONE_CRYPT_KEY).into(), iv.as_slice().into());
    cipher.apply_keystream(&mut buf);

    let mut output = Vec::with_capacity(16 + buf.len());
    output.extend_from_slice(&iv);
    output.extend_from_slice(&buf);

    Ok(URL_SAFE_NO_PAD.encode(&output))
}

// ============ INI Parser ============

/// A parsed rclone remote: section name → key/value pairs.
type RcloneRemote = HashMap<String, String>;

/// Maximum number of remotes to parse.
///
/// A 10 MB config (the caller's cap) holds hundreds of thousands of minimal
/// `[name]\ntype=ftp\npass=x` sections, and each imported credential costs a
/// full read-modify-rewrite of the vault, so an uncapped parse turns one file
/// into a quadratic write storm. Matches the cap the other importers use.
const MAX_REMOTES: usize = 10_000;

/// Parse rclone.conf INI format into named sections.
fn parse_rclone_conf(content: &str) -> HashMap<String, RcloneRemote> {
    let mut sections: HashMap<String, RcloneRemote> = HashMap::new();
    let mut current_section: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // Section header: [name]
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim().to_string();
            if !name.is_empty() {
                if !sections.contains_key(&name) && sections.len() >= MAX_REMOTES {
                    log::warn!("rclone import: stopped at the {MAX_REMOTES}-remote cap");
                    break;
                }
                sections.entry(name.clone()).or_default();
                current_section = Some(name);
            }
            continue;
        }

        // Key = value
        if let Some(ref section) = current_section {
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_lowercase();
                let value = value.trim().to_string();
                if let Some(sec) = sections.get_mut(section) {
                    sec.insert(key, value);
                }
            }
        }
    }

    sections
}

// ============ Type Mapping ============

#[derive(Default)]
struct MappedProfile {
    /// Why a credential the remote carried was left out, e.g. an `IsPassword`
    /// value that does not reveal. The profile still imports, without it, and
    /// the reason reaches `RcloneImportResult::warnings`.
    credential_warning: Option<String>,
    protocol: String,
    provider_id: Option<String>,
    host: String,
    port: u32,
    username: String,
    password: Option<String>,
    options: Option<serde_json::Value>,
    initial_path: Option<String>,
    /// AeroFTP-format `StoredTokens` JSON converted from the rclone
    /// `token = {...}` blob. The vault writer pairs this with the new
    /// per-profile id once the import is materialised. Issue #214.
    oauth_token: Option<String>,
    /// Jottacloud refresh-token JSON in the shape `JottacloudProvider`
    /// persists (`refresh_token`/`token_endpoint`/`username`), bound to the
    /// new per-profile vault key on import. Issue #214.
    jotta_refresh: Option<String>,
    /// rclone `protondrive` cached session converted to the JSON the native
    /// Proton Drive client keeps under `proton_native_session_<id>`.
    proton_session: Option<String>,
}

/// The `proton_mode` an imported Proton Drive profile gets: the native client
/// when this build has it, else the official CLI (which keeps its own session,
/// so an imported rclone session is then left out).
fn imported_proton_mode() -> &'static str {
    if cfg!(feature = "proton-native") {
        "native"
    } else {
        "cli"
    }
}

/// Key of the native Proton Drive session in the options the rclone export
/// path hands to [`export_rclone`] (never in a saved profile).
pub const PROTON_SESSION_OPTION: &str = "proton_native_session";

/// rclone's `protondrive` cached session as the JSON the native Proton Drive
/// client stores (`StoredSession`: `uid`, `access_token`, `refresh_token`,
/// `key_password`). rclone keeps `client_uid`, `client_access_token`,
/// `client_refresh_token` and `client_salted_key_pass`, the last being the
/// key password that unlocks the account keys (the same secret the browser
/// sign-in hands over), base64-encoded by Proton-API-Bridge
/// (`common/user.go`). `None` unless all four are present and the key
/// password decodes.
fn rclone_proton_session_to_aeroftp(remote: &RcloneRemote) -> Option<String> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    let field = |key: &str| remote.get(key).map(|v| v.trim()).filter(|v| !v.is_empty());
    let uid = field("client_uid")?;
    let access_token = field("client_access_token")?;
    let refresh_token = field("client_refresh_token")?;
    let key_password = STANDARD
        .decode(field("client_salted_key_pass")?)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|password| !password.is_empty())?;
    Some(
        serde_json::json!({
            "uid": uid,
            "access_token": access_token,
            "refresh_token": refresh_token,
            "key_password": key_password,
        })
        .to_string(),
    )
}

/// The reverse of [`rclone_proton_session_to_aeroftp`]: the `client_*` lines
/// rclone's `protondrive` backend reads its cached session from. `None` when
/// the session lacks a field rclone needs (it requires all four).
fn aeroftp_proton_session_to_rclone(session_json: &str) -> Option<[(&'static str, String); 4]> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    let value: serde_json::Value = serde_json::from_str(session_json).ok()?;
    let field = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    Some([
        ("client_uid", field("uid")?),
        ("client_access_token", field("access_token")?),
        ("client_refresh_token", field("refresh_token")?),
        (
            "client_salted_key_pass",
            STANDARD.encode(field("key_password")?.as_bytes()),
        ),
    ])
}

/// Convert an rclone `token = {...}` blob to the JSON shape AeroFTP's vault
/// stores under `oauth_<provider>` keys (`StoredTokens`). rclone uses
/// RFC 3339 `"expiry"` while AeroFTP uses a Unix `expires_at`; the
/// conversion is otherwise field-for-field. Returns `None` when the blob
/// lacks `access_token` or is not valid JSON. Issue #214.
fn rclone_token_to_aeroftp(blob: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(blob).ok()?;
    let access_token = value.get("access_token")?.as_str()?.to_string();
    if access_token.is_empty() {
        return None;
    }
    let refresh_token = value
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let token_type = value
        .get("token_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Bearer")
        .to_string();
    let expires_at = value
        .get("expiry")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp());
    let aeroftp = serde_json::json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "expires_at": expires_at,
        "token_type": token_type,
        "scopes": serde_json::Value::Array(Vec::new()),
    });
    serde_json::to_string_pretty(&aeroftp).ok()
}

/// Inverse of [`rclone_token_to_aeroftp`]: convert AeroFTP's stored
/// `StoredTokens` JSON (Unix `expires_at`) into the rclone `token = {...}`
/// blob (RFC 3339 `expiry`) the OAuth backends expect. Returns `None` when the
/// stored value lacks a usable `access_token`. `token_type` defaults to
/// `Bearer`; `refresh_token` and `expiry` are emitted only when present (rclone
/// treats a missing expiry as "refresh on first use", the safe default for an
/// exported token). Issue #128-D.
fn aeroftp_token_to_rclone(blob: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(blob).ok()?;
    let access_token = value.get("access_token")?.as_str()?.to_string();
    if access_token.is_empty() {
        return None;
    }
    let token_type = value
        .get("token_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Bearer")
        .to_string();
    let mut out = serde_json::Map::new();
    out.insert(
        "access_token".into(),
        serde_json::Value::String(access_token),
    );
    out.insert("token_type".into(), serde_json::Value::String(token_type));
    if let Some(refresh) = value
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        out.insert(
            "refresh_token".into(),
            serde_json::Value::String(refresh.to_string()),
        );
    }
    if let Some(expiry) = value
        .get("expires_at")
        .and_then(|v| v.as_i64())
        .and_then(|ts| chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0))
    {
        out.insert(
            "expiry".into(),
            serde_json::Value::String(expiry.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        );
    }
    serde_json::to_string(&serde_json::Value::Object(out)).ok()
}

/// rclone's zoho backend sends `Authorization: <token_type> <access_token>`.
/// Zoho's API accepts `Zoho-oauthtoken`, not `Bearer`. AeroFTP stores the
/// latter (our connect path rewrites the header). Force the rclone type so
/// an exported remote does not get F6016 "URL Rule is not configured".
fn zoho_rclone_token_type(token_json: &str) -> String {
    let mut value: serde_json::Value = match serde_json::from_str(token_json) {
        Ok(v) => v,
        Err(_) => return token_json.to_string(),
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "token_type".into(),
            serde_json::Value::String("Zoho-oauthtoken".into()),
        );
    }
    serde_json::to_string(&value).unwrap_or_else(|_| token_json.to_string())
}

/// Append the rclone OAuth credential block for an OAuth-token backend.
///
/// AeroFTP mints its OAuth tokens with the user's own (BYO) OAuth app, so
/// rclone can refresh them only when the config carries the SAME
/// `client_id`/`client_secret`; rclone's built-in app cannot. The export path
/// injects all three from the vault into the profile `options` under private
/// `__aeroftp_oauth_*` keys. When every piece is present we emit a usable,
/// refreshable remote; otherwise we emit a guidance comment instead of a
/// silently broken half-remote. Issue #128-D.
///
/// The guidance goes to `notes`, which the exporter writes above the
/// `[remote]` header: rclone keeps a comment with the section that follows
/// it, so a comment left inside the body moves under the NEXT remote the
/// first time rclone rewrites the file (a reconnect does exactly that).
fn push_oauth_credentials(
    output: &mut String,
    notes: &mut String,
    remote_name: &str,
    options: Option<&serde_json::Value>,
) {
    push_oauth_credentials_inner(output, notes, remote_name, options, None);
}

fn push_oauth_credentials_inner(
    output: &mut String,
    notes: &mut String,
    remote_name: &str,
    options: Option<&serde_json::Value>,
    force_token_type: Option<&str>,
) {
    let get = |k: &str| {
        options
            .and_then(|o| o.get(k))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let token = get("__aeroftp_oauth_token")
        .and_then(aeroftp_token_to_rclone)
        .map(|tok| match force_token_type {
            Some("Zoho-oauthtoken") => zoho_rclone_token_type(&tok),
            _ => tok,
        });
    let client_id = get("__aeroftp_oauth_client_id");
    let client_secret = get("__aeroftp_oauth_client_secret");
    match (token, client_id, client_secret) {
        (Some(tok), Some(cid), Some(csec)) => {
            output.push_str(&format!("client_id = {}\n", cid));
            output.push_str(&format!("client_secret = {}\n", csec));
            output.push_str(&format!("token = {}\n", tok));
        }
        _ => {
            notes.push_str(&format!(
                "# OAuth credentials not exported (client_id/client_secret/token\n\
                 # unavailable in the vault). Run `rclone config reconnect {}`\n\
                 # to authorize this remote before use.\n",
                rclone_shell_arg(remote_name, true)
            ));
        }
    }
}

/// Build the Jotta refresh-token blob in the shape `JottacloudProvider`
/// persists locally (issue #214). rclone stores `username`, `token = {...}`
/// (with the same fields as OAuth2 providers) and a `client_id`. We extract
/// the refresh token and rebuild the persistence shape so reconnecting on
/// the destination device skips the single-use login token.
fn rclone_jotta_to_aeroftp(remote: &RcloneRemote) -> Option<String> {
    let token_blob = remote.get("token")?;
    let token: serde_json::Value = serde_json::from_str(token_blob).ok()?;
    let refresh_token = token
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?;
    let username = remote.get("user").map(|s| s.as_str()).unwrap_or("");
    // rclone's jottacloud backend uses the JAR token endpoint by default;
    // surface it the same way `JottacloudProvider` does after a successful
    // reconnect, but leave it empty when rclone did not store it so the
    // provider re-discovers OIDC on first use.
    let token_endpoint = remote
        .get("token_endpoint")
        .map(|s| s.as_str())
        .unwrap_or("");
    let blob = serde_json::json!({
        "refresh_token": refresh_token,
        "token_endpoint": token_endpoint,
        "username": username,
    });
    serde_json::to_string(&blob).ok()
}

// Provider tables now live in `crate::bridge_shared` (Refactor 6): a single
// source for every importer instead of per-module duplicates.

/// An rclone backend the bridge carries, by rclone's own `type` name.
#[cfg(test)]
pub(crate) struct RcloneBridgeBackend {
    pub rclone_type: &'static str,
    /// How the docs name it; `None` keeps a backend out of the published
    /// figures (Google Photos: the provider is on hold, import only).
    pub label: Option<&'static str>,
    /// Whether `export_rclone` writes it (every listed type imports).
    pub exported: bool,
}

/// Every rclone backend type the bridge maps, the one list the published
/// figures come from (README, CLI guide). `crypt` and `alias` are not storage
/// backends and stay out: they wrap one of these. Tests pin this list to the
/// `type = ...` lines `export_rclone` writes, to the arms `map_remote` reads,
/// and to the docs, so none of them can drift alone again (the README said
/// 17 while the code carried 24).
#[cfg(test)]
pub(crate) const RCLONE_BRIDGE_BACKENDS: &[RcloneBridgeBackend] = &[
    backend("ftp", "FTP"),
    backend("sftp", "SFTP"),
    backend("s3", "S3"),
    backend("webdav", "WebDAV"),
    backend("drive", "Google Drive"),
    backend("dropbox", "Dropbox"),
    backend("onedrive", "OneDrive"),
    backend("mega", "MEGA"),
    backend("internxt", "Internxt"),
    backend("filen", "Filen"),
    backend("box", "Box"),
    backend("pcloud", "pCloud"),
    backend("azureblob", "Azure Blob"),
    backend("swift", "Swift"),
    backend("yandex", "Yandex Disk"),
    backend("zoho", "Zoho WorkDrive"),
    backend("koofr", "Koofr"),
    backend("jottacloud", "Jottacloud"),
    backend("b2", "Backblaze B2"),
    backend("opendrive", "OpenDrive"),
    backend("drime", "Drime"),
    backend("cloudinary", "Cloudinary"),
    backend("imagekit", "ImageKit"),
    backend("protondrive", "Proton Drive"),
    RcloneBridgeBackend {
        rclone_type: "gphotos",
        label: None,
        exported: false,
    },
];

#[cfg(test)]
const fn backend(rclone_type: &'static str, label: &'static str) -> RcloneBridgeBackend {
    RcloneBridgeBackend {
        rclone_type,
        label: Some(label),
        exported: true,
    }
}

/// The backends the docs count: listed, exported and imported.
#[cfg(test)]
fn rclone_published_backends() -> impl Iterator<Item = &'static RcloneBridgeBackend> {
    RCLONE_BRIDGE_BACKENDS
        .iter()
        .filter(|b| b.label.is_some() && b.exported)
}

/// A boolean option as rclone reads it, through Go's `strconv.ParseBool`:
/// `1 t T TRUE true True` and `0 f F FALSE false False`, nothing else. Reading
/// only `true` and `1` left `no_check_certificate = TRUE` verifying and
/// `tls = True` in cleartext.
fn rclone_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// The longest `key_file` an expansion may produce: Linux's `PATH_MAX`. A
/// longer path names no file on any platform AeroFTP runs on.
const MAX_RCLONE_KEY_PATH_BYTES: usize = 4096;

/// A `key_file` path as rclone opens it: environment variables expanded
/// (`${HOME}`, `$HOME`, unset ones empty, as Go's `os.ExpandEnv`), with
/// `RCLONE_CONFIG_DIR` the folder of the file being read, which rclone sets
/// for its own config. A leading `~` is left to the key loader, which expands
/// it. Kept literal, `${HOME}/.ssh/id_ed25519` named no file and the profile
/// stopped signing in.
/// `None` when the result would pass [`MAX_RCLONE_KEY_PATH_BYTES`]: the
/// length is checked before each piece is added, so `${PATH}` repeated a
/// million times stops at the cap instead of allocating hundreds of MB.
fn expand_rclone_path(path: &str, config_dir: Option<&Path>) -> Option<String> {
    let lookup = |name: &str| -> String {
        if name == "RCLONE_CONFIG_DIR" {
            if let Some(dir) = config_dir {
                return dir.to_string_lossy().into_owned();
            }
        }
        std::env::var(name).unwrap_or_default()
    };
    let is_name = |c: char| c == '_' || c.is_ascii_alphanumeric();
    let mut out = String::with_capacity(path.len().min(MAX_RCLONE_KEY_PATH_BYTES));
    let push = |out: &mut String, piece: &str| -> Option<()> {
        (out.len() + piece.len() <= MAX_RCLONE_KEY_PATH_BYTES).then(|| out.push_str(piece))
    };
    let mut rest = path;
    while let Some(at) = rest.find('$') {
        push(&mut out, &rest[..at])?;
        let after = &rest[at + 1..];
        if let Some(braced) = after.strip_prefix('{') {
            match braced.find('}') {
                Some(end) => {
                    push(&mut out, &lookup(&braced[..end]))?;
                    rest = &braced[end + 1..];
                }
                None => {
                    push(&mut out, &rest[at..])?;
                    rest = "";
                }
            }
        } else {
            let end = after.find(|c: char| !is_name(c)).unwrap_or(after.len());
            if end == 0 {
                push(&mut out, "$")?;
            } else {
                push(&mut out, &lookup(&after[..end]))?;
            }
            rest = &after[end..];
        }
    }
    push(&mut out, rest)?;
    Some(out)
}

/// The folder `${RCLONE_CONFIG_DIR}` names for the file at `config_path`,
/// made absolute against the current folder. A bare `rclone.conf` (the CLI
/// passes the argument as typed) has an empty parent, and the key path
/// `${RCLONE_CONFIG_DIR}/id_ed25519` became `/id_ed25519`.
fn rclone_config_dir(config_path: &Path) -> Option<std::path::PathBuf> {
    std::path::absolute(config_path)
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

/// Expand every `key_file` the way rclone does ([`expand_rclone_path`]) once,
/// where the folder of the file is known.
fn expand_rclone_key_files(
    sections: &mut HashMap<String, RcloneRemote>,
    config_dir: Option<&Path>,
) -> Result<(), String> {
    for (name, remote) in sections.iter_mut() {
        if let Some(key_file) = remote.get_mut("key_file") {
            if key_file.contains('$') {
                *key_file = expand_rclone_path(key_file, config_dir).ok_or_else(|| {
                    format!(
                        "the key_file of '{}' expands past {} bytes, which names no file; \
                         check its environment variables",
                        name.replace(['\n', '\r'], " "),
                        MAX_RCLONE_KEY_PATH_BYTES
                    )
                })?;
            }
        }
    }
    Ok(())
}

/// Convert a single rclone remote to an AeroFTP profile.
/// `aeroftp_export` is what the header says when AeroFTP's own export wrote the
/// file ([`aeroftp_export_header`]), `None` for any other rclone.conf.
fn map_remote(
    name: &str,
    remote: &RcloneRemote,
    aeroftp_export: Option<AeroftpExport>,
) -> Result<MappedProfile, String> {
    let rclone_type = remote
        .get("type")
        .ok_or_else(|| "missing type field".to_string())?
        .to_lowercase();

    // An rclone `IsPassword` field. A value that does not reveal, or reveals
    // to nothing, is left out, and why goes to the import report through
    // `credential_notes`.
    let credential_notes = std::cell::RefCell::new(Vec::new());
    let note = |text: String| credential_notes.borrow_mut().push(text);
    let get_password = |key: &str| -> Option<String> {
        match rclone_password_field(remote, key) {
            RcloneSecretField::Absent => None,
            RcloneSecretField::Revealed(secret) => Some(secret),
            RcloneSecretField::Empty => {
                note(format!(
                    "{key} reveals to an empty value; imported without it"
                ));
                None
            }
            RcloneSecretField::Unreadable(why) => {
                note(format!(
                    "{key} does not reveal as an rclone-obscured password ({why}); imported without it"
                ));
                None
            }
        }
    };

    let get_str = |key: &str| remote.get(key).map(|s| s.as_str());
    // A secret rclone writes as it is, in a field that is not `IsPassword`
    // (S3 `secret_access_key`, the Azure, Swift and B2 `key`, the Drime,
    // Cloudinary and ImageKit keys): taken verbatim. Through the reveal codec,
    // a key that happens to decode came back as the noise it decodes to, and
    // was stored with no warning.
    let get_plain_secret = |key: &str| get_str(key).filter(|v| !v.is_empty()).map(str::to_string);
    // The same, for a field AeroFTP's own export once wrote obscured. Whether
    // the file came from such a version is read from its header: the version
    // when it names one, otherwise the date. rclone keeps that header when it
    // rewrites the file, so a key fixed later by hand can sit in an old export
    // in plain text, and a plain key can decode too. A reveal is therefore
    // taken only when it gives a key's shape (printable ASCII, no spaces),
    // and always reported; an empty one means no key.
    let get_aeroftp_plain_secret = |key: &str, (until, fixed_in): (&str, &str)| {
        let value = get_plain_secret(key)?;
        let Some(export) = aeroftp_export else {
            return Some(value);
        };
        let fixed = parse_aeroftp_version(fixed_in).expect("a constant version");
        let until = chrono::DateTime::parse_from_rfc3339(until)
            .expect("a constant RFC 3339 time")
            .with_timezone(&chrono::Utc);
        let written_obscured = match (export.version, export.exported) {
            (Some(version), _) => version < fixed,
            (None, Some(exported)) => exported < until,
            // A header with neither says nothing: read the file as rclone does.
            (None, None) => return Some(value),
        };
        let Ok(revealed) = reveal_rclone_password(&value) else {
            return Some(value);
        };
        let key_shaped = revealed.len() >= MIN_REVEALED_PLAIN_SECRET_LEN
            && revealed.bytes().all(|b| (0x21..=0x7e).contains(&b));
        if !written_obscured {
            if key_shaped && export.version.is_none() {
                note(format!(
                    "{key} also reads as an rclone-obscured value, the form AeroFTP \
                     exports before {fixed_in} wrote; kept as written: if the profile \
                     does not sign in, export it again from a current AeroFTP"
                ));
            }
            return Some(value);
        }
        if revealed.is_empty() {
            // A 22-character value in the rclone alphabet decodes to an IV and
            // nothing else, so it reveals to empty whether it is obscure("")
            // or a clear key of that length. Both are left out, and the note
            // names both causes: calling every such value an obscured empty
            // key is wrong when the user typed a real 22-character key.
            note(format!(
                "{key} is 22 characters and reveals to an empty value. That is true of \
                 an obscured empty key and of a clear key of that length, so the two \
                 cannot be told apart; imported without it. If this was a real key, \
                 set it again"
            ));
            return None;
        }
        if !key_shaped {
            // A real old Swift password can contain a space or a non-ASCII
            // character, and a short clear key can reveal to one printable
            // byte. Neither is stored as the revealed text. The note says so;
            // the ciphertext stays as written, and the value itself stays out
            // of the note.
            note(format!(
                "{key} reveals to a value this import does not take (shorter than \
                 {MIN_REVEALED_PLAIN_SECRET_LEN} characters, or it contains a space or a \
                 non-ASCII character), so it was kept as written. AeroFTP exports before \
                 {fixed_in} wrote it obscured; if the profile does not sign in, set this \
                 key again"
            ));
            return Some(value);
        }
        note(format!(
            "{key} was revealed: AeroFTP exports before {fixed_in} wrote it obscured; \
             if the profile does not sign in, check this key"
        ));
        Some(revealed)
    };
    let get_port = |key: &str, default: u32| -> u32 {
        remote
            .get(key)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(default)
    };

    let mapped = match rclone_type.as_str() {
        // ---- FTP ----
        "ftp" => {
            let host = get_str("host").unwrap_or("").to_string();
            if host.is_empty() {
                return Err("ftp remote has no host".to_string());
            }
            let flag = |k: &str| get_str(k).and_then(rclone_bool).unwrap_or(false);
            // rclone's `tls` is implicit FTPS, `explicit_tls` is AUTH TLS on 21.
            // Keep which one: an `ftps` profile without a mode is opened (and
            // exported back) as implicit, which an explicit server refuses.
            let tls_mode = if flag("tls") {
                Some("implicit")
            } else if flag("explicit_tls") {
                Some("explicit")
            } else {
                None
            };
            let protocol = if tls_mode.is_some() { "ftps" } else { "ftp" };
            let default_port = if tls_mode == Some("implicit") {
                990
            } else {
                21
            };

            Ok(MappedProfile {
                protocol: protocol.to_string(),
                provider_id: None,
                host,
                port: get_port("port", default_port),
                username: get_str("user").unwrap_or("anonymous").to_string(),
                password: get_password("pass"),
                options: tls_mode.map(|m| serde_json::json!({ "tlsMode": m })),
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- SFTP ----
        "sftp" => {
            let host = get_str("host").unwrap_or("").to_string();
            if host.is_empty() {
                return Err("sftp remote has no host".to_string());
            }
            Ok(MappedProfile {
                protocol: "sftp".to_string(),
                provider_id: None,
                host,
                port: get_port("port", 22),
                username: get_str("user").unwrap_or("root").to_string(),
                password: get_password("pass"),
                // The key the export writes as `key_file` (a path, never the
                // key bytes) and its obscured passphrase. Dropped here, a
                // key-auth profile came back password-only and stopped
                // signing in (measured on 2026-11-06: seven SFTP profiles).
                options: {
                    let mut options = serde_json::Map::new();
                    if let Some(key_file) =
                        get_str("key_file").map(str::trim).filter(|k| !k.is_empty())
                    {
                        options.insert(
                            "private_key_path".into(),
                            serde_json::Value::String(key_file.to_string()),
                        );
                    }
                    if let Some(pass) = get_password("key_file_pass") {
                        options.insert("key_passphrase".into(), serde_json::Value::String(pass));
                    }
                    (!options.is_empty()).then_some(serde_json::Value::Object(options))
                },
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- S3 ----
        "s3" => {
            let s3_provider = get_str("provider").unwrap_or("Other");
            let provider_id = crate::bridge_shared::map_s3_provider(s3_provider);
            let endpoint = get_str("endpoint").unwrap_or("").to_string();
            // rclone's `provider = Other` carries no vendor signal; refine via the
            // endpoint host so known presets (Filebase, IDrive e2, ...) land on
            // their provider id instead of the generic `custom-s3` bucket.
            let provider_id = if provider_id == "custom-s3" && !endpoint.is_empty() {
                crate::bridge_shared::map_s3_provider_from_endpoint(&endpoint)
            } else {
                provider_id
            };
            // A remote with no region is `us-east-1` everywhere but on Tigris,
            // whose own default (and the preset's) is `auto`.
            let region = match get_str("region") {
                Some(region) if provider_id != "tigris" || !region.trim().is_empty() => {
                    region.to_string()
                }
                _ if provider_id == "tigris" => "auto".to_string(),
                _ => "us-east-1".to_string(),
            };

            // Build S3 endpoint host
            let host = if !endpoint.is_empty() {
                endpoint
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .trim_end_matches('/')
                    .to_string()
            } else if provider_id == "amazon-s3" {
                format!("s3.{}.amazonaws.com", region)
            } else {
                // Generic S3: need endpoint
                return Err("s3 remote has no endpoint".to_string());
            };

            let mut options = serde_json::Map::new();
            if let Some(bucket) = get_str("bucket") {
                if !bucket.is_empty() {
                    options.insert(
                        "bucket".into(),
                        serde_json::Value::String(bucket.to_string()),
                    );
                }
            }
            options.insert("region".into(), serde_json::Value::String(region));
            if let Some(ep) = get_str("endpoint") {
                if !ep.is_empty() {
                    options.insert("endpoint".into(), serde_json::Value::String(ep.to_string()));
                }
            }
            // Path-style access (common for non-AWS)
            let path_style = get_str("force_path_style")
                .or(get_str("use_path_style"))
                .and_then(rclone_bool)
                .unwrap_or(provider_id != "amazon-s3");
            options.insert("pathStyle".into(), serde_json::Value::Bool(path_style));

            Ok(MappedProfile {
                protocol: "s3".to_string(),
                provider_id: Some(provider_id.to_string()),
                host,
                port: 443,
                username: get_str("access_key_id").unwrap_or("").to_string(),
                password: get_aeroftp_plain_secret(
                    "secret_access_key",
                    AEROFTP_OBSCURED_S3_AZURE_UNTIL,
                ),
                options: Some(serde_json::Value::Object(options)),
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- WebDAV ----
        "webdav" => {
            let url = get_str("url").unwrap_or("").to_string();
            if url.is_empty() {
                return Err("webdav remote has no url".to_string());
            }
            let vendor = get_str("vendor").unwrap_or("other");
            let provider_id = crate::bridge_shared::map_webdav_vendor(vendor);

            // Parse URL to extract host and base path
            let (host, base_path, port) = parse_webdav_url(&url);

            let mut options = serde_json::Map::new();
            if !base_path.is_empty() {
                options.insert("basePath".into(), serde_json::Value::String(base_path));
            }
            // The scheme the url names, which the connect reads as
            // `webdavScheme` (`tls_mode`): without it `http://nas:8080`
            // came back as HTTPS on 8080, at connect and in the next export.
            let scheme = url
                .trim()
                .split_once("://")
                .map(|(s, _)| s.to_ascii_lowercase());
            if let Some(scheme @ ("http" | "https")) = scheme.as_deref() {
                options.insert(
                    "webdavScheme".into(),
                    serde_json::Value::String(scheme.to_string()),
                );
            }

            Ok(MappedProfile {
                protocol: "webdav".to_string(),
                provider_id: Some(provider_id.to_string()),
                host,
                port,
                username: get_str("user").unwrap_or("").to_string(),
                password: get_password("pass"),
                options: if options.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(options))
                },
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- Google Drive ----
        "drive" => Ok(MappedProfile {
            protocol: "googledrive".to_string(),
            provider_id: Some("googledrive".to_string()),
            host: "www.googleapis.com".to_string(),
            port: 443,
            username: name.to_string(), // rclone doesn't store email for drive
            password: None,
            options: None,
            // rclone writes `root_folder_id =` blank for the account root (the
            // parser has already trimmed the value).
            initial_path: get_str("root_folder_id")
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            // Issue #214: import the OAuth token blob so the destination
            // device reconnects without a re-auth round-trip, symmetrically
            // with how `.aeroftp` exports carry credentials.
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
            proton_session: None,
            credential_warning: None,
        }),

        // ---- Google Photos ----
        "gphotos" | "googlephotos" => Ok(MappedProfile {
            protocol: "googlephotos".to_string(),
            provider_id: Some("googlephotos".to_string()),
            host: "photoslibrary.googleapis.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
            proton_session: None,
            credential_warning: None,
        }),

        // ---- Dropbox ----
        "dropbox" => Ok(MappedProfile {
            protocol: "dropbox".to_string(),
            provider_id: Some("dropbox".to_string()),
            host: "api.dropboxapi.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
            proton_session: None,
            credential_warning: None,
        }),

        // ---- OneDrive ----
        // A SharePoint document library: AeroFTP's OneDrive opens the signed-in
        // user's own drive (`/me/drive`), so the profile would read and write
        // there instead of in the library the remote names.
        "onedrive"
            if get_str("drive_type")
                .map(str::trim)
                .is_some_and(|t| t.eq_ignore_ascii_case("documentLibrary")) =>
        {
            Err(
                "onedrive remote opens a SharePoint document library, and AeroFTP's OneDrive \
                 opens the signed-in user's own drive"
                    .to_string(),
            )
        }
        "onedrive" => Ok(MappedProfile {
            protocol: "onedrive".to_string(),
            provider_id: Some("onedrive".to_string()),
            host: "graph.microsoft.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None,
            // The drive the remote is bound to, which the export writes from
            // the same keys: without them the profile exported again gave
            // rclone a remote that fails with "unable to get drive_id".
            options: {
                let mut options = serde_json::Map::new();
                for key in ["drive_id", "drive_type", "region"] {
                    if let Some(value) = get_str(key).map(str::trim).filter(|v| !v.is_empty()) {
                        options.insert(key.into(), serde_json::Value::String(value.to_string()));
                    }
                }
                (!options.is_empty()).then_some(serde_json::Value::Object(options))
            },
            initial_path: None,
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
            proton_session: None,
            credential_warning: None,
        }),

        // ---- MEGA ----
        "mega" => Ok(MappedProfile {
            protocol: "mega".to_string(),
            provider_id: Some("mega".to_string()),
            host: "mega.nz".to_string(),
            port: 443,
            username: get_str("user").unwrap_or("").to_string(),
            password: get_password("pass"),
            options: None,
            initial_path: None,
            oauth_token: None,
            jotta_refresh: None,
            proton_session: None,
            credential_warning: None,
        }),

        // ---- Internxt ----
        // `email` + obscured `pass`, the pair AeroFTP signs in with. The host is
        // the one the GUI gives a new Internxt profile. After its own sign-in
        // rclone also stores `mnemonic` (obscured) and `token`: both are
        // ignored, since AeroFTP derives the mnemonic and a token at every
        // sign-in and keeps neither on disk.
        "internxt" => {
            let email = get_str("email").unwrap_or("").to_string();
            if email.is_empty() {
                return Err("internxt remote has no email".to_string());
            }
            Ok(MappedProfile {
                protocol: "internxt".to_string(),
                provider_id: Some("internxt".to_string()),
                host: "gateway.internxt.com".to_string(),
                port: 443,
                username: email,
                password: get_password("pass"),
                options: None,
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- Filen ----
        // rclone's `filen` backend stores `email` + obscured `password` +
        // obscured `api_key` (all three required), plus advanced keys it derives
        // during `rclone config` (`master_keys`, `auth_version`, ...). AeroFTP
        // needs the account password (it re-derives the master keys / E2E
        // material from it on connect) and can optionally take the CLI api_key,
        // which skips the /v3/login 2FA window (issue #230). We import both; the
        // advanced rclone keys are ignored because AeroFTP re-derives them. The
        // api_key lands in `options.filen_api_key`, which the save path relocates
        // to the vault under `filen_api_key_<id>`.
        "filen" => {
            let email = get_str("email").unwrap_or("").to_string();
            if email.is_empty() {
                return Err("filen remote has no email".to_string());
            }
            let mut options = serde_json::Map::new();
            if let Some(api_key) = get_password("api_key").filter(|k| !k.is_empty()) {
                options.insert("filen_api_key".into(), serde_json::Value::String(api_key));
            }
            Ok(MappedProfile {
                protocol: "filen".to_string(),
                provider_id: Some("filen".to_string()),
                host: "filen.io".to_string(),
                port: 443,
                username: email,
                password: get_password("password"),
                options: if options.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(options))
                },
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- Box ----
        "box" => Ok(MappedProfile {
            protocol: "box".to_string(),
            provider_id: Some("box".to_string()),
            host: "api.box.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
            proton_session: None,
            credential_warning: None,
        }),

        // ---- pCloud ----
        // rclone's pcloud `hostname` defaults to api.pcloud.com (US); only EU
        // accounts carry eapi.pcloud.com. Derive the region so the imported
        // profile dials the right API instead of silently defaulting to US.
        "pcloud" => {
            let hostname = get_str("hostname").unwrap_or("api.pcloud.com");
            let region = if hostname.to_ascii_lowercase().contains("eapi") {
                "eu"
            } else {
                "us"
            };
            let mut options = serde_json::Map::new();
            options.insert(
                "region".into(),
                serde_json::Value::String(region.to_string()),
            );
            Ok(MappedProfile {
                protocol: "pcloud".to_string(),
                provider_id: Some("pcloud".to_string()),
                host: hostname.to_string(),
                port: 443,
                username: name.to_string(),
                password: None,
                options: Some(serde_json::Value::Object(options)),
                initial_path: None,
                oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- Azure Blob Storage ----
        "azureblob" => {
            let account = get_str("account").unwrap_or("").to_string();
            if account.is_empty() {
                return Err("azureblob remote has no account".to_string());
            }
            let mut options = serde_json::Map::new();
            if let Some(container) = get_str("container") {
                options.insert(
                    "bucket".into(),
                    serde_json::Value::String(container.to_string()),
                );
            }

            Ok(MappedProfile {
                protocol: "azure".to_string(),
                provider_id: Some("azure-blob".to_string()),
                host: format!("{}.blob.core.windows.net", account),
                port: 443,
                username: account,
                password: get_aeroftp_plain_secret("key", AEROFTP_OBSCURED_S3_AZURE_UNTIL),
                options: if options.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(options))
                },
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- OpenStack Swift ----
        "swift" => {
            let auth_url = get_str("auth").unwrap_or("").to_string();
            let host = auth_url.clone();
            let port = url::Url::parse(&auth_url)
                .ok()
                .and_then(|url| url.port_or_known_default())
                .unwrap_or(443);
            if host.is_empty() {
                return Err("swift remote has no auth URL".to_string());
            }

            let mut options = serde_json::Map::new();
            if let Some(container) = get_str("container") {
                options.insert(
                    "bucket".into(),
                    serde_json::Value::String(container.to_string()),
                );
            }
            if !auth_url.is_empty() {
                options.insert("endpoint".into(), serde_json::Value::String(auth_url));
            }
            if let Some(region) = get_str("region") {
                options.insert(
                    "region".into(),
                    serde_json::Value::String(region.to_string()),
                );
            }
            if let Some(tenant) = get_str("tenant").or(get_str("tenant_id")) {
                options.insert(
                    "tenant".into(),
                    serde_json::Value::String(tenant.to_string()),
                );
            }

            // Blomp's preset carries a fact about the service (its object store
            // answers in cleartext on 8080, `preset_publishes_cleartext_storage`),
            // so a Blomp remote must come back as Blomp, not as custom Swift.
            let provider_id = if url::Url::parse(&host)
                .ok()
                .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
                .is_some_and(|h| h == "blomp.com" || h.ends_with(".blomp.com"))
            {
                "blomp"
            } else {
                "custom-swift"
            };
            Ok(MappedProfile {
                protocol: "swift".to_string(),
                provider_id: Some(provider_id.to_string()),
                host,
                port: u32::from(port),
                username: get_str("user").unwrap_or("").to_string(),
                password: get_aeroftp_plain_secret("key", AEROFTP_OBSCURED_SWIFT_UNTIL),
                options: if options.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(options))
                },
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- Yandex Disk ----
        // rclone's backend type is `yandex`; older configs and some forks used
        // `yandexdisk`. Accept both so a real `rclone config` remote imports.
        "yandex" | "yandexdisk" => Ok(MappedProfile {
            protocol: "yandexdisk".to_string(),
            provider_id: Some("yandex-disk".to_string()),
            host: "webdav.yandex.ru".to_string(),
            port: 443,
            username: name.to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
            proton_session: None,
            credential_warning: None,
        }),

        // ---- Zoho WorkDrive ----
        // rclone's backend type is `zoho`. Region uses rclone's TLD slugs
        // (`com`, `com.au`); AeroFTP stores `us` / `au`. Map them so a
        // re-imported profile dials the same data centre it exported from.
        "zoho" => {
            let rclone_region = get_str("region").unwrap_or("com");
            let region = zoho_region_from_rclone(rclone_region);
            let mut options = serde_json::Map::new();
            options.insert("region".into(), serde_json::Value::String(region));
            if let Some(folder) = get_str("root_folder_id").filter(|s| !s.is_empty()) {
                options.insert(
                    "root_folder_id".into(),
                    serde_json::Value::String(folder.to_string()),
                );
            }
            Ok(MappedProfile {
                protocol: "zohoworkdrive".to_string(),
                provider_id: Some("zoho-workdrive".to_string()),
                host: "workdrive.zoho.com".to_string(),
                port: 443,
                username: name.to_string(),
                password: None,
                options: Some(serde_json::Value::Object(options)),
                // rclone's root_folder_id is a workspace/privatespace id, not
                // an AeroFTP path. Putting it in initial_path made `ls /<id>`
                // fail after a re-import. The id lives only on options.
                initial_path: None,
                oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- Koofr ----
        "koofr" => Ok(MappedProfile {
            protocol: "koofr".to_string(),
            provider_id: Some("koofr".to_string()),
            host: get_str("endpoint")
                .unwrap_or("https://app.koofr.net")
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string(),
            port: 443,
            username: get_str("user").unwrap_or("").to_string(),
            password: get_password("password"),
            options: None,
            initial_path: None,
            oauth_token: None,
            jotta_refresh: None,
            proton_session: None,
            credential_warning: None,
        }),

        // ---- Jottacloud ----
        "jottacloud" => Ok(MappedProfile {
            protocol: "jottacloud".to_string(),
            provider_id: Some("jottacloud".to_string()),
            host: "jottacloud.com".to_string(),
            port: 443,
            username: get_str("user").unwrap_or(name).to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: None,
            jotta_refresh: rclone_jotta_to_aeroftp(remote),
            proton_session: None,
            credential_warning: None,
        }),

        // ---- Backblaze B2 (native API v4) ----
        // rclone stores `account` as the applicationKeyId and `key` as the
        // applicationKey. We route them to the native AeroFTP B2 provider so
        // users get large-file workflow + server-side copy + version history
        // (the previous mapping went through the S3-compatible endpoint).
        "b2" => {
            let account = get_str("account").unwrap_or("").to_string();
            if account.is_empty() {
                return Err("b2 remote has no account".to_string());
            }

            let mut options = serde_json::Map::new();
            if let Some(bucket) = get_str("bucket") {
                options.insert(
                    "bucket".into(),
                    serde_json::Value::String(bucket.to_string()),
                );
            }

            Ok(MappedProfile {
                protocol: "backblaze".to_string(),
                provider_id: Some("backblaze-native".to_string()),
                host: "api.backblazeb2.com".to_string(),
                port: 443,
                username: account,
                password: get_plain_secret("key"),
                options: Some(serde_json::Value::Object(options)),
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- OpenDrive ----
        "opendrive" => Ok(MappedProfile {
            protocol: "opendrive".to_string(),
            provider_id: Some("opendrive".to_string()),
            host: "od.lk".to_string(),
            port: 443,
            username: get_str("username").unwrap_or("").to_string(),
            password: get_password("password"),
            options: None,
            initial_path: None,
            oauth_token: None,
            jotta_refresh: None,
            proton_session: None,
            credential_warning: None,
        }),

        // ---- Drime ----
        // `access_token` is the API token AeroFTP sends as a Bearer token.
        // AeroFTP opens the default workspace from its root, and rclone never
        // writes `workspace_id` or `root_folder_id` itself: a remote carrying
        // either was scoped by hand, and imported it would open other content.
        "drime" => {
            let token = get_plain_secret("access_token")
                .ok_or_else(|| "drime remote has no access_token".to_string())?;
            if let Some(ws) = get_str("workspace_id")
                .map(str::trim)
                .filter(|w| !w.is_empty() && *w != "0")
            {
                return Err(format!(
                    "drime remote is pinned to workspace {ws}; AeroFTP opens the default workspace only"
                ));
            }
            if let Some(folder) = get_str("root_folder_id")
                .map(str::trim)
                .filter(|f| !f.is_empty())
            {
                return Err(format!(
                    "drime remote is rooted at folder id {folder}, which AeroFTP cannot open as a start folder"
                ));
            }
            Ok(MappedProfile {
                protocol: "drime".to_string(),
                provider_id: Some("drime".to_string()),
                host: "app.drime.cloud".to_string(),
                port: 443,
                username: "api-token".to_string(),
                password: Some(token),
                options: None,
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- Cloudinary ----
        // `cloud_name` lands where the app's form keeps it, `options.bucket`.
        // AeroFTP calls api.cloudinary.com only, so a remote pointed at another
        // regional endpoint through `upload_prefix` is refused.
        "cloudinary" => {
            let cloud_name = get_str("cloud_name")
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .ok_or_else(|| "cloudinary remote has no cloud_name".to_string())?;
            let api_key = get_str("api_key")
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .ok_or_else(|| "cloudinary remote has no api_key".to_string())?;
            if let Some(prefix) = get_str("upload_prefix")
                .map(|p| p.trim().trim_end_matches('/'))
                .filter(|p| !p.is_empty() && *p != "https://api.cloudinary.com")
            {
                return Err(format!(
                    "cloudinary remote uses the endpoint {prefix}; AeroFTP calls api.cloudinary.com only"
                ));
            }
            let api_secret = get_plain_secret("api_secret");
            if api_secret.is_none() {
                note("api_secret is missing; imported without it".to_string());
            }
            Ok(MappedProfile {
                protocol: "cloudinary".to_string(),
                provider_id: Some("cloudinary".to_string()),
                host: "api.cloudinary.com".to_string(),
                port: 443,
                username: api_key.to_string(),
                password: api_secret,
                options: Some(serde_json::json!({ "bucket": cloud_name })),
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- ImageKit ----
        // The URL endpoint becomes the profile's "URL Endpoint ID", which takes
        // the whole URL. The public key AeroFTP does not use is kept, so the
        // profile can be exported back to rclone, which requires it.
        "imagekit" => {
            let endpoint = get_str("endpoint")
                .map(|e| e.trim().trim_end_matches('/'))
                .filter(|e| !e.is_empty())
                .ok_or_else(|| "imagekit remote has no endpoint".to_string())?;
            let public_key = get_str("public_key")
                .map(str::trim)
                .filter(|k| !k.is_empty());
            if public_key.is_none() {
                note(
                    "public_key is missing: AeroFTP does not need it, but exporting \
                     this profile back to rclone does"
                        .to_string(),
                );
            }
            let options = public_key.map(|k| serde_json::json!({ "imagekit_public_key": k }));
            let private_key = get_plain_secret("private_key");
            if private_key.is_none() {
                note("private_key is missing; imported without it".to_string());
            }
            Ok(MappedProfile {
                protocol: "imagekit".to_string(),
                provider_id: Some("imagekit".to_string()),
                host: "api.imagekit.io".to_string(),
                port: 443,
                username: endpoint.to_string(),
                password: private_key,
                options,
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: None,
                credential_warning: None,
            })
        }

        // ---- Proton Drive ----
        // The account password (and mailbox password, 2FA seed) is never
        // imported: the native client signs in through Proton's browser page
        // and never holds one. The cached session rclone keeps comes along,
        // so the profile connects at once; from then on the two share it, and
        // Proton rotates the refresh token, so the first to refresh keeps the
        // session (rclone then logs in again with its password, AeroFTP asks
        // for a new sign-in).
        "protondrive" => {
            let mode = imported_proton_mode();
            let session = (mode == "native")
                .then(|| rclone_proton_session_to_aeroftp(remote))
                .flatten();
            let warning = if mode != "native" {
                "this build reaches Proton Drive through the official CLI: sign in with `proton-drive auth login`; rclone's saved session was left out"
            } else if session.is_some() {
                "rclone's saved session was imported and is now shared with rclone: the first of the two to refresh it keeps it, the other signs in again"
            } else {
                concat!(
                    "the remote holds no complete saved session: after the import, ",
                    crate::providers::proton_sign_in_hint!()
                )
            };
            Ok(MappedProfile {
                protocol: "proton".to_string(),
                provider_id: None,
                host: "drive.proton.me".to_string(),
                port: 443,
                username: get_str("username").unwrap_or(name).to_string(),
                password: None,
                options: Some(serde_json::json!({ "proton_mode": mode })),
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
                proton_session: session,
                credential_warning: Some(warning.to_string()),
            })
        }

        // ---- FileLu ----
        // The remote holds the FileLu Rclone key, which rclone sends to
        // filelu.com/rclone. AeroFTP signs in to filelu.com/api with the
        // account's Developer API key, a different key the remote does not hold.
        "filelu" => Err(
            "filelu remote holds the FileLu Rclone key, not the Developer API key \
             AeroFTP signs in with (Account Settings)"
                .to_string(),
        ),

        // Unsupported rclone types: skip gracefully
        _ => Err(format!("unsupported rclone type: {}", rclone_type)),
    };
    let mut mapped = mapped?;
    // The way back of the export's certificate switch: the FTP backend's own
    // `no_check_certificate`, the `override.` form the export writes for the
    // backends without one, and the bare key older AeroFTP files carry. Without
    // it a profile that only connects with verification off came back
    // verifying. In a file AeroFTP did not write, the bare key counts only
    // where rclone reads it (FTP): a WebDAV, S3 or Swift remote holding it is
    // verified by rclone ([`RCLONE_OVERRIDE_NO_CHECK_CERT`]), and turning the
    // check off on import exposed its credentials to a man in the middle.
    let check_off = |k: &str| get_str(k).and_then(rclone_bool).unwrap_or(false);
    let bare_key_counts = rclone_type == "ftp" || aeroftp_export.is_some();
    let cert_check_off = check_off("override.no_check_certificate")
        || (bare_key_counts && check_off("no_check_certificate"));
    if cert_check_off && matches!(rclone_type.as_str(), "ftp" | "webdav" | "s3" | "swift") {
        let options = mapped
            .options
            .get_or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let Some(map) = options.as_object_mut() {
            map.insert("verifyCert".into(), serde_json::Value::Bool(false));
        }
    }
    Ok(with_credential_notes(mapped, credential_notes.into_inner()))
}

/// Map AeroFTP's Zoho region slug to rclone's `zoho` backend `region`.
///
/// rclone interpolates the value as the TLD in `https://accounts.zoho.{region}`
/// (see `setupRegion` in rclone's zoho backend). Documented examples are
/// `com` / `eu` / `in` / `jp` / `com.cn` / `com.au`. `uk`, `sa`, and `ae`
/// fit the same template and are real Zoho data centres, so they are
/// emitted. Canada lives at `zohocloud.ca`, which that template cannot
/// express: return `None` rather than a host rclone will never reach.
/// Unknown slugs are also `None` so we never write `region = garbage`.
fn zoho_region_to_rclone(region: &str) -> Option<String> {
    match region.trim().to_ascii_lowercase().as_str() {
        "" | "us" | "com" => Some("com".into()),
        "au" | "com.au" => Some("com.au".into()),
        "cn" | "com.cn" => Some("com.cn".into()),
        "eu" | "in" | "jp" | "uk" | "sa" | "ae" => Some(region.trim().to_ascii_lowercase()),
        // `ca` / `zohocloud.ca`: rclone would build accounts.zoho.ca.
        _ => None,
    }
}

/// Inverse of [`zoho_region_to_rclone`]: rclone TLD -> AeroFTP slug.
fn zoho_region_from_rclone(region: &str) -> String {
    match region.trim().to_ascii_lowercase().as_str() {
        "" | "com" | "us" => "us".into(),
        "com.au" | "au" => "au".into(),
        "com.cn" | "cn" => "cn".into(),
        other => other.to_string(),
    }
}

/// Whether `name` is made only of the characters rclone allows in a remote
/// name (`[A-Za-z0-9_.+@ -]`, ASCII). Such a text names a section and carries
/// no parameter, so a message may repeat it.
fn is_rclone_remote_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '@' | ' ' | '-'))
}

fn parse_crypt_remote_target(remote_target: &str) -> (String, Option<String>) {
    if let Some((base, subpath)) = remote_target.split_once(':') {
        let normalized = subpath.trim_end().trim_start_matches('/');
        let initial_path = if normalized.is_empty() {
            None
        } else {
            Some(format!("/{}", normalized))
        };
        (base.trim().to_string(), initial_path)
    } else {
        (remote_target.trim().to_string(), None)
    }
}

/// The target a crypt's `remote = <name>:<path>` means once every `alias`
/// remote in front of it is replaced by what it points at: `alias:sub` is
/// `<the alias's remote>/sub`. This exporter writes a crypt over an alias for
/// a profile that starts in a subfolder or pins a bucket, so its own export
/// imports only with this. A target that is not an alias comes back as it
/// is; a chain that does not end is refused with the crypt.
fn resolve_alias_target(
    remote_target: &str,
    sections: &HashMap<String, RcloneRemote>,
) -> Result<String, String> {
    let mut target = remote_target.to_string();
    for _ in 0..8 {
        let Some((name, sub)) = target.split_once(':') else {
            return Ok(target);
        };
        let Some(alias) = sections
            .get(name.trim())
            .filter(|section| section.get("type").map(|t| t.trim()) == Some("alias"))
        else {
            return Ok(target);
        };
        let inner = alias.get("remote").map(|r| r.trim()).unwrap_or("");
        if inner.is_empty() {
            return Err(format!("the alias '{}' points at no remote", name.trim()));
        }
        let sub = sub.trim_end().trim_start_matches('/');
        target = if sub.is_empty() {
            inner.to_string()
        } else if inner.ends_with(':') || inner.ends_with('/') {
            format!("{inner}{sub}")
        } else {
            format!("{inner}/{sub}")
        };
    }
    Err("a chain of alias remotes that does not end".to_string())
}

/// An rclone `alias` remote: its target remote, opened at a folder. It maps to
/// the target's profile with that folder as the start (a bucket for S3), so a
/// remote someone pointed at `nas:/team` arrives as a profile that opens in
/// `/team`. Reported as an unsupported type before, which dropped every start
/// folder and pinned bucket AeroFTP's own export carries this way (measured on
/// 2026-11-06: 49 alias remotes in one export of 118 profiles).
fn map_alias_remote(
    name: &str,
    remote: &RcloneRemote,
    sections: &HashMap<String, RcloneRemote>,
    aeroftp_export: Option<AeroftpExport>,
) -> Result<(MappedProfile, String), String> {
    let target = remote
        .get("remote")
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .ok_or_else(|| "alias remote points at no remote".to_string())?;
    let resolved = resolve_alias_target(target, sections)?;
    let Some((base, path)) = resolved.split_once(':') else {
        return Err("alias remote points at a local folder, which is not a server".to_string());
    };
    let base = base.trim();
    // rclone also takes an on-the-fly backend (`:s3,...:bucket`) and a
    // connection string (`mys3,secret_access_key=...:bucket`) here. Their
    // parameters can be secrets, so the reason names at most the remote in
    // front of them, as for a crypt ([`map_crypt_remote`]); echoed whole, the
    // secret reached the import report.
    if base.is_empty() {
        return Err(
            "alias remote opens an on-the-fly backend, which AeroFTP cannot carry".to_string(),
        );
    }
    if let Some((named, _parameters)) = base.split_once(',') {
        return Err(if is_rclone_remote_name(named) {
            format!(
                "alias remote opens '{named}' with connection-string parameters, which AeroFTP cannot carry"
            )
        } else {
            "alias remote opens a backend with connection-string parameters, which AeroFTP cannot carry"
                .to_string()
        });
    }
    if !is_rclone_remote_name(base) {
        return Err("alias remote points at a target that is not a named remote".to_string());
    }
    let base_remote = sections
        .get(base)
        .ok_or_else(|| format!("alias remote points at '{base}', which is not in this file"))?;
    if base_remote.get("type").map(|t| t.trim()) == Some("crypt") {
        return Err(format!(
            "alias remote opens the crypt remote '{base}', which imports on its own"
        ));
    }
    let mut mapped = map_remote(name, base_remote, aeroftp_export)?;
    let alias_has_path = !path.trim_end().trim_matches('/').is_empty();
    // A Drive remote rooted at a folder id (`root_folder_id`, which the profile
    // keeps as its start) reads the alias path under that folder; written as a
    // path it opened at the account root instead.
    if mapped.protocol == "googledrive"
        && mapped
            .initial_path
            .as_deref()
            .is_some_and(|root| !root.is_empty())
        && alias_has_path
    {
        return Err(format!(
            "alias remote opens a folder inside the Drive folder '{base}' is rooted at \
             (root_folder_id), which AeroFTP cannot address by path"
        ));
    }
    apply_rclone_start_folder(&mut mapped, path);
    Ok((mapped, base.to_string()))
}

/// Make the folder an alias opens the profile's start: the bucket and the
/// path under it for S3, the absolute folder for SFTP (`nas:/abs`), and a
/// path under the URL's collection for WebDAV, whose start folder is a path
/// on the server. Elsewhere the alias path is under the account root.
fn apply_rclone_start_folder(mapped: &mut MappedProfile, path: &str) {
    // rclone's sftp reads `nas:docs` from the login folder and `nas:/docs`
    // from the root. AeroFTP opens a start folder without a leading `/` from
    // the root too, so a folder under the login one is written `~/docs`, the
    // form the provider resolves against it; `nas:/` is the root, which
    // AeroFTP opens for a start folder of `/` (no start folder is the login
    // folder). A space after the `:` is part of the folder rclone opens
    // (`nas: docs` is ` docs`, measured with rclone v1.75.2); its config
    // drops only the trailing ones, with the rest of the value.
    if mapped.protocol == "sftp" {
        let path = path.trim_end();
        let kept = path.trim_end_matches('/');
        if kept.starts_with('/') {
            mapped.initial_path = Some(kept.to_string());
        } else if !kept.is_empty() {
            mapped.initial_path = Some(format!("~/{kept}"));
        } else if path.starts_with('/') {
            mapped.initial_path = Some("/".to_string());
        }
        return;
    }
    let rel = path.trim_end().trim_matches('/');
    if rel.is_empty() {
        return;
    }
    let options = mapped
        .options
        .get_or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let start = match mapped.protocol.as_str() {
        // Swift and Azure containers and B2 buckets sit where an S3 bucket
        // does. rclone has no bucket key for these backends (a `container`
        // line is ignored), so the first segment of the path IS the bucket
        // rclone opens, even when the section names one.
        "s3" | "swift" | "azure" | "backblaze" => {
            let (bucket, rest) = rel.split_once('/').unwrap_or((rel, ""));
            if let Some(map) = options.as_object_mut() {
                map.insert(
                    "bucket".into(),
                    serde_json::Value::String(bucket.to_string()),
                );
            }
            rest.trim_matches('/').to_string()
        }
        "webdav" => {
            let base = options
                .get("basePath")
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .trim_matches('/');
            if base.is_empty() {
                rel.to_string()
            } else {
                format!("{base}/{rel}")
            }
        }
        _ => rel.to_string(),
    };
    if !start.is_empty() {
        mapped.initial_path = Some(format!("/{start}"));
    }
}

fn map_crypt_remote(
    name: &str,
    remote: &RcloneRemote,
    sections: &HashMap<String, RcloneRemote>,
    aeroftp_export: Option<AeroftpExport>,
) -> Result<(MappedProfile, String), String> {
    let remote_target = remote
        .get("remote")
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    if remote_target.is_empty() {
        return Err("crypt remote has no remote to wrap".to_string());
    }
    let remote_target = resolve_alias_target(&remote_target, sections)?;

    if remote_target.starts_with(':') {
        return Err(
            "crypt remote wraps an on-the-fly backend, which AeroFTP cannot carry".to_string(),
        );
    }
    let (base_remote_name, crypt_subpath) = parse_crypt_remote_target(&remote_target);
    // rclone also takes a connection string (`mys3,secret_access_key=...:bucket`),
    // an on-the-fly backend (`:s3,...:bucket`) and a local path here. Their
    // parameters can be secrets and AeroFTP has nowhere to keep them, so such a
    // crypt is not imported, and the reason names at most the remote in front
    // of the parameters, never the parameters.
    if let Some((named, _parameters)) = base_remote_name.split_once(',') {
        return Err(if is_rclone_remote_name(named) {
            format!(
                "crypt remote wraps '{}' with connection-string parameters, which AeroFTP cannot carry",
                named
            )
        } else {
            "crypt remote wraps a backend with connection-string parameters, which AeroFTP cannot carry"
                .to_string()
        });
    }
    if !is_rclone_remote_name(&base_remote_name) {
        return Err(
            "crypt remote wraps a local path or another target that is not a named remote, \
             which AeroFTP cannot carry"
                .to_string(),
        );
    }
    let base_remote = sections.get(&base_remote_name).ok_or_else(|| {
        format!(
            "crypt remote wraps '{}', which is not in this file",
            base_remote_name
        )
    })?;
    let mut mapped = map_remote(&base_remote_name, base_remote, aeroftp_export)
        .map_err(|e| format!("crypt remote wraps '{}': {}", base_remote_name, e))?;

    let get_str = |k: &str| remote.get(k).map(|s| s.as_str());
    // `password` and `password2` are `IsPassword` fields. A crypt remote whose
    // password or salt rclone could not reveal is not imported: the overlay
    // would be enabled with a key derived from something else, and an
    // unreadable salt left out would select rclone's default salt, so files
    // would read as noise, or be written under a key rclone cannot open. An
    // absent salt, or `obscure("")`, IS rclone's default salt. An empty
    // password is valid in rclone, which then uses an all-zero key (rclone
    // v1.75.1 `backend/crypt/cipher.go`, `Key`), but the overlay requires a
    // password, so that remote is not imported either.
    let password =
        match rclone_password_field(remote, "password") {
            RcloneSecretField::Absent => None,
            RcloneSecretField::Revealed(password) => Some(password),
            RcloneSecretField::Empty => return Err(
                "crypt remote's password is empty (rclone then encrypts with an all-zero key), \
                 and AeroFTP's crypt overlay requires a password"
                    .to_string(),
            ),
            RcloneSecretField::Unreadable(why) => {
                return Err(format!(
                "crypt remote's password does not reveal as an rclone-obscured password ({why})"
            ))
            }
        };
    let salt = match rclone_password_field(remote, "password2") {
        RcloneSecretField::Absent | RcloneSecretField::Empty => None,
        RcloneSecretField::Revealed(salt) => Some(salt),
        RcloneSecretField::Unreadable(why) => {
            return Err(format!(
                "crypt remote's password2 (the salt) does not reveal as an rclone-obscured \
                 password ({why}); without it the overlay would use rclone's default salt"
            ))
        }
    };

    let mut options = match mapped.options.take() {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };

    options.insert("rcloneCryptEnabled".into(), serde_json::Value::Bool(true));
    options.insert(
        "rcloneCryptRemote".into(),
        serde_json::Value::String(remote_target.clone()),
    );
    options.insert(
        "rcloneCryptOverlayName".into(),
        serde_json::Value::String(name.to_string()),
    );

    if let Some(pw) = password {
        options.insert("rcloneCryptPassword".into(), serde_json::Value::String(pw));
    }
    if let Some(pw2) = salt {
        options.insert(
            "rcloneCryptPassword2".into(),
            serde_json::Value::String(pw2),
        );
    }
    if let Some(mode) = get_str("filename_encryption") {
        options.insert(
            "rcloneCryptFilenameEncryption".into(),
            serde_json::Value::String(mode.to_string()),
        );
    }
    if let Some(dir_enc) = get_str("directory_name_encryption").and_then(rclone_bool) {
        options.insert(
            "rcloneCryptDirectoryNameEncryption".into(),
            serde_json::Value::Bool(dir_enc),
        );
    }

    mapped.options = Some(serde_json::Value::Object(options));

    // rclone's sftp opens `nas:docs` under the login folder and `nas:/docs`
    // from the root, as an alias does ([`apply_rclone_start_folder`]); the
    // folder is also the overlay's scope. Read from the root like the other
    // backends, `nas: docs` became `/ docs`, another folder, where files were
    // encrypted with the right key and rclone never looked.
    if mapped.protocol == "sftp" {
        let raw = remote_target.split_once(':').map_or("", |(_, path)| path);
        apply_rclone_start_folder(&mut mapped, raw);
    } else if crypt_subpath.is_some() {
        mapped.initial_path = crypt_subpath;
    }

    Ok((mapped, base_remote_name))
}

/// Parse a WebDAV URL into (host, basePath, port).
fn parse_webdav_url(url: &str) -> (String, String, u32) {
    let without_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let is_https = url.starts_with("https://");

    let (host_port, path) = match without_scheme.find('/') {
        Some(i) => (&without_scheme[..i], without_scheme[i..].to_string()),
        None => (without_scheme, String::new()),
    };

    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => {
            let port = p.parse::<u32>().unwrap_or(if is_https { 443 } else { 80 });
            (h.to_string(), port)
        }
        None => (host_port.to_string(), if is_https { 443 } else { 80 }),
    };

    (host, path, port)
}

// ============ Default config path detection ============

/// Returns the default rclone.conf path for the current platform.
pub fn default_rclone_config_path() -> Option<PathBuf> {
    // rclone uses $RCLONE_CONFIG env var first
    if let Ok(path) = std::env::var("RCLONE_CONFIG") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Some(p);
        }
    }

    // Try `rclone config file` command output (most reliable).
    // #351: hidden_command avoids a console window flash on Windows.
    if let Ok(output) = crate::hidden_command("rclone")
        .args(["config", "file"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Output is like: "Configuration file is stored at:\n/path/to/rclone.conf\n"
            for line in stdout.lines() {
                let line = line.trim();
                if line.ends_with("rclone.conf") || line.ends_with("rclone.conf\"") {
                    let path = PathBuf::from(line.trim_matches('"'));
                    if path.exists() {
                        return Some(path);
                    }
                }
            }
        }
    }

    // Platform-specific defaults
    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".config/rclone/rclone.conf");
            if path.exists() {
                return Some(path);
            }
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let path = PathBuf::from(xdg).join("rclone/rclone.conf");
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".config/rclone/rclone.conf");
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let path = PathBuf::from(appdata).join("rclone/rclone.conf");
            if path.exists() {
                return Some(path);
            }
        }
    }

    None
}

// ============ Public API ============

/// Result of importing rclone config.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RcloneImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<RcloneSkippedRemote>,
    pub warnings: Vec<RcloneImportWarning>,
    pub source_path: String,
    pub total_remotes: usize,
    /// Per-profile OAuth / Jotta token blobs imported from the rclone
    /// `[remote]` sections, keyed by the freshly minted AeroFTP profile id.
    /// `#[serde(skip)]` keeps them out of the renderer-bound response: the
    /// vault writer in `lib.rs` consumes them server-side and only the
    /// `hasStoredCredential` boolean reaches the frontend. Issue #214.
    #[serde(skip)]
    pub provider_secrets: std::collections::HashMap<String, crate::profile_export::ProviderSecrets>,
}

impl RcloneImportResult {
    /// The secrets in the order of the servers, not of the map, which changes from
    /// run to run: of two remotes with different OAuth apps, the first one decides
    /// which app stays stored. Refreshable tokens come first: their refresh
    /// works only with their own app. Access-only tokens follow, then app-only
    /// remotes; equal classes keep the config's order.
    pub fn secrets_in_server_order(
        &self,
    ) -> Vec<(&String, &crate::profile_export::ProviderSecrets)> {
        let mut secrets: Vec<_> = self
            .servers
            .iter()
            .filter_map(|server| Some((&server.id, self.provider_secrets.get(&server.id)?)))
            .collect();
        secrets.sort_by_key(|(_, secret)| {
            let refreshable = secret
                .oauth
                .as_deref()
                .and_then(|token| serde_json::from_str::<serde_json::Value>(token).ok())
                .and_then(|token| {
                    token
                        .get("refresh_token")
                        .and_then(|value| value.as_str())
                        .map(|refresh| !refresh.trim().is_empty())
                })
                .unwrap_or(false);
            (!refreshable, secret.oauth.is_none())
        });
        secrets
    }
}

/// A remote that was skipped, with the reason.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RcloneSkippedRemote {
    pub name: String,
    pub rclone_type: String,
    pub reason: String,
}

/// Option keys the importer fills with a revealed secret: the rclone-crypt
/// password and salt (`map_crypt_remote`) and the Filen CLI API key
/// (`map_remote`). The save paths move them into the vault; whatever prints an
/// import (the CLI `import rclone --json` report) leaves them out, as the GUI
/// preview does.
pub const IMPORT_SECRET_OPTION_KEYS: &[&str] = &[
    "rcloneCryptPassword",
    "rcloneCryptPassword2",
    "filen_api_key",
    // The SFTP key passphrase a profile keeps in its options, which the
    // rclone exporter writes as `key_file_pass` (review of #980).
    "key_passphrase",
    // The native Proton Drive session the export path injects.
    PROTON_SESSION_OPTION,
];

/// `options` without the keys in [`IMPORT_SECRET_OPTION_KEYS`], for output.
pub fn options_without_secrets(options: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let mut options = options?.clone();
    if let Some(map) = options.as_object_mut() {
        for key in IMPORT_SECRET_OPTION_KEYS {
            map.remove(*key);
        }
    }
    Some(options)
}

/// A remote that imported, but not whole: e.g. its password did not reveal,
/// so the profile carries no credential. Kept apart from `skipped`, which
/// lists remotes that did not import at all.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RcloneImportWarning {
    pub name: String,
    pub reason: String,
}

/// What one import may hold in memory once aliases and crypts are expanded.
/// Each of them is mapped from the section it opens, so its secrets are copied
/// again: 10,000 aliases over one section with a 1 MiB secret would hold about
/// 10 GiB from a 2 MiB file (Codex review RCB-02, 2026-11-06).
const MAX_RCLONE_EXPANDED_BYTES: usize = 64 * 1024 * 1024;

/// Refuse a file whose remotes would expand past [`MAX_RCLONE_EXPANDED_BYTES`].
/// A remote costs its own keys and values, and an alias or crypt also costs
/// every section on its way to the remote it opens (at most 8 hops, the bound
/// `resolve_alias_target` keeps).
fn check_rclone_expansion(sections: &HashMap<String, RcloneRemote>) -> Result<(), String> {
    let size =
        |remote: &RcloneRemote| -> usize { remote.iter().map(|(k, v)| k.len() + v.len()).sum() };
    let mut total = 0usize;
    for remote in sections.values() {
        total = total.saturating_add(size(remote));
        let mut current = remote;
        for _ in 0..8 {
            let wraps = matches!(
                current.get("type").map(|t| t.trim()),
                Some("alias") | Some("crypt")
            );
            // `remote = base` with no colon opens the whole of `base`, as
            // `parse_crypt_remote_target` reads it, so it copies `base` too.
            let Some(base) = wraps
                .then(|| current.get("remote"))
                .flatten()
                .map(|r| r.trim())
                .map(|r| r.split_once(':').map_or(r, |(base, _)| base).trim())
            else {
                break;
            };
            let Some(next) = sections.get(base) else {
                break;
            };
            total = total.saturating_add(size(next));
            current = next;
        }
        if total > MAX_RCLONE_EXPANDED_BYTES {
            return Err(format!(
                "rclone.conf expands to more than {} MB once its aliases and crypt remotes are \
                 resolved; split the file",
                MAX_RCLONE_EXPANDED_BYTES / (1024 * 1024)
            ));
        }
    }
    Ok(())
}

/// Import all supported remotes from an rclone.conf file.
/// Whether `name` is one the export generates for an alias of `base` opening
/// `path`: `<base>-path` for a start folder, `<base>-<bucket>` for a pinned
/// bucket of a bucket backend (the path is then the bucket alone), either with
/// the `-2`.. suffix
/// a taken name gets ([`RcloneNamespace::claim_generated`]). Matched on the
/// `<base>-` prefix alone, a `nas-backup` alias written by hand into the file
/// was folded into `nas`, which then opened in its folder, and disappeared.
fn is_generated_alias_name(name: &str, base: &str, base_type: &str, path: &str) -> bool {
    let start_folder = sanitize_rclone_remote_name(&format!("{base}-path"));
    let bucket = path.trim_end();
    let bucket_backend = matches!(base_type, "s3" | "swift" | "azureblob" | "b2");
    let bucket_alias = (bucket_backend && !bucket.is_empty() && !bucket.contains('/'))
        .then(|| s3_bucket_alias_name(base, bucket))
        .flatten();
    [Some(start_folder), bucket_alias]
        .into_iter()
        .flatten()
        .any(|generated| {
            name == generated
                || name
                    .strip_prefix(&format!("{generated}-"))
                    .and_then(|n| n.parse::<usize>().ok())
                    .is_some_and(|n| (2..=MAX_NAME_SUFFIX).contains(&n))
        })
}

pub fn import_rclone(config_path: &Path) -> Result<RcloneImportResult, String> {
    // Read through the cap, not after a size check: a file that grows between
    // the caller's metadata check and this read cannot get past it.
    let content = {
        use std::io::Read;
        let cap = crate::bridge_shared::MAX_IMPORT_FILE_BYTES;
        let mut bytes = Vec::new();
        std::fs::File::open(config_path)
            .and_then(|f| f.take(cap + 1).read_to_end(&mut bytes))
            .map_err(|e| format!("Read rclone.conf: {}", e))?;
        if bytes.len() as u64 > cap {
            return Err(format!(
                "rclone.conf is larger than {} MB",
                cap / (1024 * 1024)
            ));
        }
        String::from_utf8(bytes).map_err(|e| format!("Read rclone.conf: {}", e))?
    };

    let mut sections = parse_rclone_conf(&content);
    expand_rclone_key_files(&mut sections, rclone_config_dir(config_path).as_deref())?;
    check_rclone_expansion(&sections)?;
    let aeroftp_export = aeroftp_export_header(&content);
    let total_remotes = sections.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();
    let mut warnings = Vec::new();
    let mut provider_secrets: std::collections::HashMap<
        String,
        crate::profile_export::ProviderSecrets,
    > = std::collections::HashMap::new();

    // Iterate remotes in a stable, alphabetical order. parse_rclone_conf
    // returns a HashMap whose iteration order is randomized per run, which
    // surfaced the imported servers (and the skipped list) in a different
    // order every time. Sorting the section names by their lexicographic
    // order mirrors `rclone listremotes`, so the import list matches what
    // the user sees in rclone itself.
    let mut section_names: Vec<&String> = sections.keys().collect();
    section_names.sort();

    // AeroFTP's own export carries a profile's start folder or pinned bucket
    // as an alias it names after the profile (`<name>-path`, `<name>-<bucket>`).
    // Those fold back into the profile they came from, so the round trip
    // returns one profile that opens where it did, not a profile at the root
    // plus a second one. Aliases in any other file are profiles of their own.
    let mut folded: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut start_of: HashMap<&str, &str> = HashMap::new();
    if aeroftp_export.is_some() {
        for name in &section_names {
            let remote = &sections[*name];
            if remote.get("type").map(|t| t.trim()) != Some("alias") {
                continue;
            }
            let Some((base, path)) = remote.get("remote").and_then(|r| r.trim().split_once(':'))
            else {
                continue;
            };
            let base = base.trim();
            let base_type = sections
                .get(base)
                .and_then(|b| b.get("type"))
                .map(|t| t.trim())
                .unwrap_or("");
            let base_is_plain = !matches!(base_type, "" | "alias" | "crypt");
            // A folder inside a Drive rooted at a folder id cannot be a path
            // (see `map_alias_remote`): folded, the start folder replaced the
            // root id and `gd:Team` opened /Team at the account root. Left as
            // an alias, the guard there skips it with the reason.
            let rooted_drive = base_type == "drive"
                && sections
                    .get(base)
                    .and_then(|b| b.get("root_folder_id"))
                    .is_some_and(|id| !id.trim().is_empty());
            let alias_has_path = !path.trim_end().trim_matches('/').is_empty();
            if base_is_plain
                && !(rooted_drive && alias_has_path)
                && is_generated_alias_name(name, base, base_type, path)
                && !start_of.contains_key(base)
            {
                start_of.insert(base, path);
                folded.insert(name.as_str());
            }
        }
    }

    for name in section_names {
        let remote = &sections[name];
        let rclone_type = remote.get("type").map(|s| s.as_str()).unwrap_or("unknown");
        if folded.contains(name.as_str()) {
            continue;
        }

        // The section holding the OAuth app (`client_id`, `client_secret`):
        // for an alias or a crypt, the remote it opens, whose token was mapped.
        let mut app_section = remote;
        let mapped = if rclone_type == "crypt" {
            map_crypt_remote(name, remote, &sections, aeroftp_export).map(|(mapped, base)| {
                if let Some(base_remote) = sections.get(&base) {
                    app_section = base_remote;
                }
                mapped
            })
        } else if rclone_type == "alias" {
            map_alias_remote(name, remote, &sections, aeroftp_export).map(|(mapped, base)| {
                if let Some(base_remote) = sections.get(&base) {
                    app_section = base_remote;
                }
                mapped
            })
        } else {
            map_remote(name, remote, aeroftp_export).map(|mut mapped| {
                if let Some(path) = start_of.get(name.as_str()) {
                    apply_rclone_start_folder(&mut mapped, path);
                }
                mapped
            })
        };

        match mapped {
            Ok(mapped) => {
                if let Some(reason) = &mapped.credential_warning {
                    tracing::warn!("[rclone import] remote '{}': {}", name, reason);
                    warnings.push(RcloneImportWarning {
                        name: name.clone(),
                        reason: reason.clone(),
                    });
                }
                let id = format!(
                    "rclone-{}-{}",
                    name.to_lowercase().replace(' ', "-"),
                    &crate::bridge_shared::uuid_v4()[..8]
                );

                // Issue #214: capture per-profile OAuth / Jotta blobs so the
                // vault writer can store them under the new per-profile key.
                // #128-D: also recover the BYO OAuth app client_id/secret that
                // rclone stores in plain in the remote section, so a fresh
                // device can refresh the imported token (rclone refreshes an
                // AeroFTP-minted token only with the same OAuth app).
                // An app with no token yet still belongs to the vault: a later sign-in
                // on this device needs it.
                let pick = |k: &str| {
                    app_section
                        .get(k)
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                };
                let (oauth_client_id, oauth_client_secret) = if mapped.oauth_token.is_some()
                    || crate::bridge_commands::rclone_oauth_client_cred_key(&mapped.protocol)
                        .is_some()
                {
                    (pick("client_id"), pick("client_secret"))
                } else {
                    (None, None)
                };
                if mapped.oauth_token.is_some()
                    || mapped.jotta_refresh.is_some()
                    || mapped.proton_session.is_some()
                    || oauth_client_id.is_some()
                    || oauth_client_secret.is_some()
                {
                    provider_secrets.insert(
                        id.clone(),
                        crate::profile_export::ProviderSecrets {
                            oauth: mapped.oauth_token,
                            jotta_refresh: mapped.jotta_refresh,
                            proton_native_session: mapped.proton_session,
                            oauth_client_id,
                            oauth_client_secret,
                            ..Default::default()
                        },
                    );
                }

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
                    options: mapped.options,
                    provider_id: mapped.provider_id,
                    credential: mapped.password,
                    has_stored_credential: None,
                    public_url_base: None,
                    ..Default::default()
                });
            }
            Err(reason) => {
                skipped.push(RcloneSkippedRemote {
                    name: name.clone(),
                    rclone_type: rclone_type.to_string(),
                    reason,
                });
            }
        }
    }

    Ok(RcloneImportResult {
        servers,
        skipped,
        warnings,
        source_path: config_path.display().to_string(),
        total_remotes,
        provider_secrets,
    })
}

// uuid_v4 now lives in `crate::bridge_shared` (Refactor 6).

// ============ Export to rclone.conf ============

/// A server profile to export as rclone remote.
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RcloneExportServer {
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
    pub provider_id: Option<String>,
    // Password is fetched from vault separately and passed in
}

/// One shell argument for an rclone command written into the exported file:
/// `"My Internxt:"` for commands that take a remote, `"My Internxt"` for
/// `rclone config update`, which takes the bare name. Names come out of
/// `sanitize_rclone_remote_name`, which keeps only ASCII `[A-Za-z0-9_.+@ -]`,
/// so a space is the only character that needs protecting and nothing in a
/// name can close or expand a double-quoted string.
fn rclone_shell_arg(remote_name: &str, with_colon: bool) -> String {
    format!("\"{}{}\"", remote_name, if with_colon { ":" } else { "" })
}

/// Sanitize a saved-profile name into a valid rclone remote name.
///
/// rclone validates remote names against `^[\w.+@ -]+$` and additionally
/// rejects a leading `-`/space and a trailing space
/// (`fspath.CheckConfigName`, `\w` being ASCII-only in Go's regexp).
/// AeroFTP profile names are free-form and routinely contain characters
/// rclone refuses, e.g. `axpbuntu-remote (admin)` (parentheses). The
/// previous export only stripped INI-breaking characters (`[ ] CR LF`),
/// so such profiles produced a config rclone could not even load. This
/// maps every disallowed character to `-`, collapses runs of separators
/// to a single `-`, and trims the edges. Names that already worked
/// (single internal spaces, e.g. `axpbuntu lab MinIO`) are left intact.
/// Returns an empty string only if nothing usable remains; the caller
/// skips those.
fn sanitize_rclone_remote_name(name: &str) -> String {
    let mapped = name.chars().map(|c| {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '@' | ' ' | '-') {
            c
        } else {
            '-'
        }
    });

    // Collapse runs of 2+ separators ([space-]) into a single '-' so a
    // replaced bracket adjacent to an existing space/dash does not leave
    // "remote -admin-". A lone space is a run of length 1 and survives.
    let mut collapsed = String::with_capacity(name.len());
    let mut sep_run = 0usize;
    for c in mapped {
        if c == ' ' || c == '-' {
            sep_run += 1;
            match sep_run {
                1 => collapsed.push(c),
                2 => {
                    collapsed.pop();
                    collapsed.push('-');
                }
                _ => {}
            }
        } else {
            sep_run = 0;
            collapsed.push(c);
        }
    }

    collapsed.trim_matches(|c| c == ' ' || c == '-').to_string()
}

/// Export server profiles to rclone.conf INI format.
/// Passwords are obscured using rclone's AES-256-CTR scheme for compatibility.
/// Strip CR/LF from a value bound for an rclone INI `key = value` line.
///
/// rclone's INI does not support multi-line values for these keys, so a profile
/// field containing a newline could otherwise inject a second `[section]` into
/// the generated `rclone.conf` (e.g. a host of `h\n[evil]\ntype = local`).
/// Stripping is loss-free for any value rclone could actually load and closes
/// the injection vector for the bridge import -> re-export round-trip.
fn ini_value(s: &str) -> String {
    s.replace(['\r', '\n'], "")
}

/// Return a copy of `server` with every free-text field (host, username, and
/// each string value in `options`) stripped of CR/LF, so the rest of the export
/// can write them verbatim without risk of `[section]` forgery. The remote name
/// is handled separately by `sanitize_rclone_remote_name`; secrets pulled from
/// the password map are wrapped with `ini_value` at their write sites.
fn sanitize_export_server(server: &RcloneExportServer) -> RcloneExportServer {
    let mut out = server.clone();
    out.host = ini_value(&server.host);
    out.username = ini_value(&server.username);
    if let Some(serde_json::Value::Object(map)) = out.options.as_mut() {
        for (key, value) in map.iter_mut() {
            if OBSCURED_EXPORT_OPTION_KEYS.contains(&key.as_str()) {
                continue;
            }
            if let serde_json::Value::String(s) = value {
                *s = ini_value(s);
            }
        }
    }
    out
}

/// Where the alias a crypt overlay wraps opens inside the clear remote: a
/// start folder (a path of the app's, like the overlay scope) or a pinned
/// bucket (which the app's paths are already inside).
#[derive(Clone, Copy, Debug)]
enum AliasRoot<'a> {
    Folder(&'a str),
    Bucket(&'a str),
}

impl<'a> AliasRoot<'a> {
    fn path(self) -> &'a str {
        match self {
            AliasRoot::Folder(path) | AliasRoot::Bucket(path) => path,
        }
    }
}

/// Options written only through `obscure_password`, whose output has no line
/// break: their bytes are the key material (a crypt password and salt derive
/// the encryption key, a passphrase unlocks the key file), so stripping a
/// CR/LF from them would export a different secret, not a safer line.
const OBSCURED_EXPORT_OPTION_KEYS: &[&str] = &[
    "rcloneCryptPassword",
    "rcloneCryptPassword2",
    "key_passphrase",
];

/// Derive rclone's `remote = <base>:<path>` for a crypt overlay.
///
/// Prefers an already-imported `rcloneCryptRemote` (`name:path` or a bare
/// path) and rewrites the name half to this export's sanitized base, so a
/// rename of the parent remote cannot leave the crypt remote pointing at a
/// section that no longer exists. Falls back to the pinned overlay scope
/// (`rcloneCryptOverlayScope` / a leading-slash path). Empty scope means
/// the whole remote: `base:`.
///
/// The imported path is from the root of the clear remote (`clear_name`),
/// while an alias base (the start folder, a pinned bucket) already points at
/// `base_root` inside it: the path is taken relative to that folder, or, when
/// it lies outside it, the crypt wraps the clear remote. Written as it was,
/// `vault:/enc` exported over the alias `vault-path = vault:/enc` became
/// `vault-path:/enc`, which is `vault:/enc/enc`. The overlay scope is already
/// relative to the start folder; when that folder has no alias in the file
/// (`folder`: its name ends in a space, or no name was free), the crypt wraps
/// the clear remote at that folder, written as its alias would hold it.
fn crypt_export_remote_target(
    base_name: &str,
    clear_name: &str,
    base_root: Option<AliasRoot<'_>>,
    folder: Option<&str>,
    options: &serde_json::Value,
) -> String {
    let trim = |p: &str| {
        p.trim()
            .trim_start_matches('/')
            .trim_end_matches('/')
            .to_string()
    };
    let target = |name: &str, path: &str| {
        if path.is_empty() {
            format!("{name}:")
        } else {
            format!("{name}:/{path}")
        }
    };
    let from_imported = options
        .get("rcloneCryptRemote")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(existing) = from_imported {
        let path = trim(&if existing.contains(':') {
            parse_crypt_remote_target(existing).1.unwrap_or_default()
        } else {
            existing.to_string()
        });
        return match base_root
            .map(|root| trim(root.path()))
            .filter(|root| !root.is_empty())
        {
            None => target(base_name, &path),
            Some(root) if path == root => target(base_name, ""),
            Some(root) => match path.strip_prefix(&format!("{root}/")) {
                Some(rest) => target(base_name, rest),
                None => target(clear_name, &path),
            },
        };
    }
    let scope = trim(
        options
            .get("rcloneCryptOverlayScope")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    // The app persists the scope as an absolute path at or under the start
    // folder (`resolveOverlayScope` in `src/utils/overlayScope.ts`), so over
    // an alias it is taken relative to that folder, like the imported path
    // above. Written as it was, the scope equal to the start folder
    // `/home/u/vault` became `nas-path:/home/u/vault`, which is the folder
    // inside itself (measured on 2026-11-06: three rclone-crypt SFTP profiles,
    // "directory not found"). A scope outside the folder is an older relative
    // one (`/vault` under `/team`) and keeps its meaning. A pinned bucket is
    // not in the scope at all: the app's paths start inside it, so `/media/vault`
    // under the bucket `media` is the key prefix `media/vault`, and stripped
    // like a folder it became `vault`, where no encrypted file is.
    let folder_root = match base_root {
        Some(AliasRoot::Folder(root)) => Some(root),
        Some(AliasRoot::Bucket(_)) | None => None,
    };
    let scope = match folder_root.map(trim).filter(|root| !root.is_empty()) {
        Some(root) if scope == root => String::new(),
        Some(root) => scope
            .strip_prefix(&format!("{root}/"))
            .map(str::to_string)
            .unwrap_or(scope),
        None => scope,
    };
    match folder.map(|folder| folder.trim_end_matches('/')) {
        Some(folder) => {
            let folder_rel = trim(folder);
            let scope = if scope == folder_rel {
                String::new()
            } else {
                scope
                    .strip_prefix(&format!("{folder_rel}/"))
                    .map(str::to_string)
                    .unwrap_or(scope)
            };
            if scope.is_empty() {
                format!("{clear_name}:{folder}")
            } else if folder.is_empty() {
                // The login folder an sftp remote opens: `nas:vault` is under
                // it, `nas:/vault` at the server root.
                format!("{clear_name}:{scope}")
            } else {
                format!("{clear_name}:{folder}/{scope}")
            }
        }
        None => target(base_name, &scope),
    }
}

/// Cloudinary's account name. The app's form keeps it in `options.bucket`;
/// a profile keyed by hand may carry it as the host instead, which is how
/// the provider reads it too (`provider_commands.rs` and the CLI's profile
/// loader).
fn cloudinary_cloud_name(server: &RcloneExportServer) -> Option<String> {
    server
        .options
        .as_ref()
        .and_then(|o| o.get("bucket"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let host = server.host.trim();
            (!host.is_empty() && host != "api.cloudinary.com").then(|| host.to_string())
        })
}

/// The ImageKit public key, which AeroFTP itself never needs (it signs with
/// the private key) and so only holds for a profile imported from an rclone
/// remote.
fn imagekit_public_key(options: Option<&serde_json::Value>) -> Option<&str> {
    options
        .and_then(|o| o.get("imagekit_public_key"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// rclone's `endpoint` is the whole URL endpoint. The app's "URL Endpoint ID"
/// field holds either the bare id or the URL pasted from the dashboard, and a
/// bare id lives under `ik.imagekit.io`.
fn imagekit_rclone_endpoint(id: &str) -> String {
    let id = id.trim().trim_end_matches('/');
    if id.starts_with("https://") || id.starts_with("http://") {
        id.to_string()
    } else {
        format!("https://ik.imagekit.io/{}", id.trim_start_matches('/'))
    }
}

/// Why a Cloudinary or ImageKit profile cannot become a working rclone
/// remote, when a field rclone needs is not on the profile. Checked before a
/// section name is claimed, like Zoho's region and root folder.
fn rclone_required_field_missing(proto: &str, server: &RcloneExportServer) -> Option<String> {
    match proto {
        // Without `cloud_name` rclone v1.75.1 still creates the remote, and
        // every listing then fails on the HTML page Cloudinary answers with.
        "cloudinary" if cloudinary_cloud_name(server).is_none() => Some(
            "the profile has no Cloudinary cloud name, which rclone's cloudinary \
             backend needs as `cloud_name`"
                .to_string(),
        ),
        // rclone v1.75.1 refuses the remote outright: "ImageKit.io public key
        // is required".
        "proton" if proton_mode_of(server) != "native" => Some(
            "AeroFTP reaches this profile through Proton's own CLI, which keeps the \
             session in its keyring, so the vault holds nothing rclone's protondrive \
             backend can use. Create the remote with `rclone config`."
                .to_string(),
        ),
        "proton" if proton_session_of(server).is_none() => Some(
            concat!(
                "the profile is not signed in to Proton: ",
                crate::providers::proton_sign_in_hint!(),
                ", then export again"
            )
            .to_string(),
        ),
        "imagekit" if imagekit_public_key(server.options.as_ref()).is_none() => Some(
            "rclone's imagekit backend requires the account public key, and this \
             profile has none: AeroFTP signs with the private key only, so a profile \
             created in the app does not store it. Create the remote with \
             `rclone config`, using the public key from the ImageKit dashboard."
                .to_string(),
        ),
        // These backends address the server through the host alone, so an empty
        // one used to be written as `url = https://` or `host = `: rclone accepts
        // the section and every call then fails with "no Host in request URL".
        "ftp" | "ftps" | "sftp" | "webdav" if server.host.trim().is_empty() => {
            Some("the profile has no host, which rclone needs to reach the server".to_string())
        }
        "swift" if swift_auth_url(server).is_none() => Some(
            "the profile has no auth URL, which rclone's swift backend needs as `auth`".to_string(),
        ),
        _ => None,
    }
}

/// The Keystone auth URL of a Swift profile, resolved the way the profile
/// connects: [`crate::profile_loader::profile_connection_extra`] (the CLI, MCP
/// and AeroCloud path) dials an `http(s)` `options.endpoint` when the host has
/// no scheme, and the host otherwise; `SwiftConfig::from_provider_config`
/// then adds `https://` to a host without one. Read from the host alone, a
/// host without a scheme and a different endpoint sent the password to a
/// service the profile never signs in to. `options.endpoint` is also read when
/// the host is empty, for profiles that carry the URL there; Blomp's preset
/// stores the URL as the host.
fn swift_auth_url(server: &RcloneExportServer) -> Option<String> {
    let profile = serde_json::json!({
        "protocol": server.protocol,
        "providerId": server.provider_id,
        "options": server.options,
    });
    let (extra, resolved) = crate::profile_loader::profile_connection_extra(
        &profile,
        crate::providers::ProviderType::Swift,
        &server.host,
    );
    let resolved = resolved.trim();
    let raw = if resolved.is_empty() {
        extra.get("endpoint").map(|e| e.trim()).unwrap_or("")
    } else {
        resolved
    };
    if raw.is_empty() {
        return None;
    }
    Some(
        if raw.starts_with("http://") || raw.starts_with("https://") {
            raw.to_string()
        } else {
            format!("https://{raw}")
        },
    )
}

/// Whether a Swift profile is Blomp: its preset id, or an auth host under
/// `blomp.com` (the same test the import uses to bring Blomp back).
fn swift_is_blomp(server: &RcloneExportServer) -> bool {
    if server.provider_id.as_deref() == Some("blomp") {
        return true;
    }
    swift_auth_url(server)
        .and_then(|auth| url::Url::parse(&auth).ok())
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|h| h == "blomp.com" || h.ends_with(".blomp.com"))
}

/// Whether a Swift auth URL names its auth version in the path (`/v1.0`,
/// `/v2.0`, `/v3`, ...), which is how rclone picks the version when
/// `auth_version` is unset.
fn swift_auth_url_has_version(auth: &str) -> bool {
    url::Url::parse(auth).ok().is_some_and(|u| {
        u.path_segments().is_some_and(|mut segments| {
            segments.any(|seg| {
                seg.strip_prefix('v')
                    .and_then(|rest| rest.chars().next())
                    .is_some_and(|c| c.is_ascii_digit())
            })
        })
    })
}

/// Whether a WebDAV host is a local mount-app bridge (Filen Desktop, MEGAcmd),
/// whose HTTP/HTTPS protocol is chosen inside that app.
fn webdav_is_local_bridge(host: &str, port: u32) -> bool {
    u16::try_from(port).is_ok_and(|port| crate::local_bridge::is_local_bridge_authority(host, port))
}

/// The scheme of the WebDAV URL written for a host saved without one. A local
/// mount-app bridge has its protocol chosen in that app, and AeroFTP resolves
/// it at connect regardless of the scheme saved in the profile (#389); an
/// offline export cannot probe, so it writes the bridges' default, plain
/// HTTP, and a note says where to change it. Elsewhere the GUI keeps the
/// user's choice in `options.webdavScheme` (`tls_mode` after normalization);
/// without one, port 80 is HTTP and the rest HTTPS.
fn webdav_export_scheme(
    host: &str,
    port: u32,
    options: Option<&serde_json::Value>,
) -> &'static str {
    if webdav_is_local_bridge(host, port) {
        return "http";
    }
    let saved = options
        .and_then(|o| o.get("webdavScheme").or_else(|| o.get("tls_mode")))
        .and_then(|v| v.as_str())
        .map(str::to_ascii_lowercase);
    match saved.as_deref() {
        Some("http") => "http",
        Some("https") => "https",
        _ if port == 80 => "http",
        _ => "https",
    }
}

/// True when the profile turned TLS certificate verification off. The GUI
/// stores `verifyCert` as a bool, and normalization can deliver it as
/// `verify_cert` and as the string "false"; every form is honoured.
fn profile_verify_cert_off(options: Option<&serde_json::Value>) -> bool {
    options
        .and_then(|o| o.get("verify_cert").or_else(|| o.get("verifyCert")))
        .map(|vc| vc.as_bool() == Some(false) || vc.as_str() == Some("false"))
        .unwrap_or(false)
}

/// The line that makes rclone skip certificate verification for one remote of a
/// backend with no such option of its own (WebDAV, S3, Swift). rclone ignores a
/// bare `no_check_certificate = true` in those sections and verifies anyway;
/// only the `override.` form applies the global flag to that remote. Measured
/// with rclone v1.75.2 on 2026-11-06 against self-signed.badssl.com: the bare
/// key still fails with `x509: certificate signed by unknown authority`, the
/// `override.` key reaches the server (WebDAV 405, S3 an XML error page).
const RCLONE_OVERRIDE_NO_CHECK_CERT: &str = "override.no_check_certificate = true\n";

/// The `proton_mode` option of a Proton Drive profile (`cli` when absent).
fn proton_mode_of(server: &RcloneExportServer) -> &str {
    server
        .options
        .as_ref()
        .and_then(|o| o.get("proton_mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("cli")
}

/// The `client_*` lines of the native session the export path injected.
fn proton_session_of(server: &RcloneExportServer) -> Option<[(&'static str, String); 4]> {
    server
        .options
        .as_ref()
        .and_then(|o| o.get(PROTON_SESSION_OPTION))
        .and_then(|v| v.as_str())
        .and_then(aeroftp_proton_session_to_rclone)
}

/// rclone's s3 backend has no `bucket` key. A pinned bucket is exported as
/// an `alias` remote `<base>-<bucket>` whose path is `<base>:<bucket>`, so
/// keys stay bucket-relative (the way AeroFTP writes them). A crypt overlay
/// on that profile must wrap the alias, not the raw s3 remote: wrapping
/// `minio:/vault` makes rclone look for a bucket named `vault`.
fn s3_pinned_bucket(options: Option<&serde_json::Value>) -> Option<&str> {
    options
        .and_then(|o| o.get("bucket"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Write the `alias` remote that opens `remote_name` inside `bucket` (an S3
/// bucket, or a Swift or Azure container, named by `what` in the comments).
/// None of these backends has a config key for it, so without the alias the
/// pinned bucket is lost. Returns the alias name written, or `None` (with a
/// note) when no name near the wanted one is free, or when rclone's config
/// cannot hold the name: it reads a value without the spaces at its end, so
/// `remote = sw:media ` would open `media`, another container.
fn write_bucket_alias(
    names: &mut RcloneNamespace,
    trailing: &mut String,
    notes: &mut String,
    remote_name: &str,
    bucket: &str,
    what: &str,
) -> Option<String> {
    if bucket.ends_with(char::is_whitespace) {
        notes.push_str(&format!(
            "# {what} alias for '{remote_name}' omitted: rclone's config cannot hold '{}', whose name ends in a space\n",
            bucket.replace(['\n', '\r'], " ")
        ));
        return None;
    }
    let desired = s3_bucket_alias_name(remote_name, bucket)?;
    // The alias name is invented by this exporter, so it yields to every
    // real profile name, including one whose section has not been written yet.
    match names.claim_generated(&desired) {
        Some(alias_name) => {
            if alias_name != desired {
                trailing.push_str(&format!(
                    "\n# renamed from '{}': that name is taken by another remote\n",
                    desired
                ));
            }
            let title = what[..1].to_ascii_uppercase() + &what[1..];
            trailing.push_str(&format!(
                "\n# {title}-relative view of '{remote_name}' ({what} '{bucket}').\n\
                 # AeroFTP writes keys at {what} root; address them\n\
                 # as `{alias_name}:` or wrap a crypt remote with `remote = {alias_name}:`.\n"
            ));
            trailing.push_str(&format!("[{}]\n", alias_name));
            trailing.push_str("type = alias\n");
            trailing.push_str(&format!("remote = {}:{}\n", remote_name, bucket));
            Some(alias_name)
        }
        None => {
            notes.push_str(&format!(
                "# {} alias for '{}' omitted: no free remote name near '{}'\n",
                what, remote_name, desired
            ));
            None
        }
    }
}

fn s3_bucket_alias_name(remote_name: &str, bucket: &str) -> Option<String> {
    let alias = sanitize_rclone_remote_name(&format!("{remote_name}-{bucket}"));
    if alias.is_empty() || alias == remote_name {
        None
    } else {
        Some(alias)
    }
}

/// What resolving a crypt section's name produced.
enum CryptName {
    /// A free name: the default `<base>-crypt`, possibly suffixed.
    Free(String),
    /// A name the operator wrote by hand that is already taken. Renaming it
    /// would silently point their `rclone sync <name>:` at something else, so
    /// the overlay is refused instead.
    Taken(String),
}

/// Resolve the name of the crypt section wrapping `base_name`.
///
/// The name the operator chose is used as written or not at all. A default
/// name is this exporter's invention and may be suffixed to get out of the way,
/// which is what the old code could not do: it checked one collision
/// (`name == base_name`) and fell back to `<base>-crypt`, exactly the form that
/// collides with a real profile called `<base>-crypt`. rclone merges duplicate
/// sections key by key, last one wins, so the crypt remote the operator
/// believed they had exported was not there and a sync to that name wrote in
/// the clear.
fn crypt_section_name(
    base_name: &str,
    profile_section: &str,
    options: &serde_json::Value,
    names: &mut RcloneNamespace,
) -> CryptName {
    let requested = sanitize_rclone_remote_name(
        options
            .get("rcloneCryptOverlayName")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    // The operator's name is used only when it is neither the clear remote's
    // section nor the base this crypt wraps. Both of those would make
    // `rclone sync <name>:` point at the crypt while that name is still the
    // clear remote. Root and subfolder fall through to `<base>-crypt` the
    // same way: in a subfolder `base` is the start-folder alias, not the
    // profile section, so comparing only with `base` used to claim the
    // profile's own name and refuse.
    if !requested.is_empty() && requested != base_name && requested != profile_section {
        return if names.claim_exact(&requested) {
            CryptName::Free(requested)
        } else {
            CryptName::Taken(requested)
        };
    }
    match names.claim_generated(&format!("{base_name}-crypt")) {
        Some(name) => CryptName::Free(name),
        None => CryptName::Taken(format!("{base_name}-crypt")),
    }
}

/// The outcome of trying to emit a profile's crypt overlay.
enum CryptSection {
    /// A section was written; it counts as a second exported remote.
    Written,
    /// This profile carries no rclone-crypt overlay.
    None,
    /// The overlay was not written, with the reason to report.
    Refused(String),
}

/// Emit a sibling `[name]` `type = crypt` section when this profile carries
/// an rclone-crypt overlay. Missing password: write the section with a
/// guidance comment instead of a silently broken `password =` line.
fn append_crypt_remote_section(
    output: &mut String,
    base_name: &str,
    profile_section: &str,
    base_root: Option<AliasRoot<'_>>,
    folder: Option<&str>,
    options: Option<&serde_json::Value>,
    names: &mut RcloneNamespace,
) -> CryptSection {
    let opts = match options {
        Some(o) if o.get("rcloneCryptEnabled").and_then(|v| v.as_bool()) == Some(true) => o,
        _ => return CryptSection::None,
    };
    // A stored crypt secret that cannot be read safely (see
    // `bridge_commands::put_stored_crypt_secrets`): no remote beats one with a
    // key AeroFTP does not use.
    if let Some(problem) = opts
        .get("rcloneCryptSecretProblem")
        .and_then(|v| v.as_str())
    {
        return CryptSection::Refused(problem.to_string());
    }
    let remote = crypt_export_remote_target(base_name, profile_section, base_root, folder, opts);
    // rclone reads `remote = nas:/data ` as `nas:/data`, another folder.
    if remote.ends_with(char::is_whitespace) {
        return CryptSection::Refused(format!(
            "it would wrap '{remote}', whose name ends in a space, which rclone's config cannot hold"
        ));
    }
    let requested = sanitize_rclone_remote_name(
        opts.get("rcloneCryptOverlayName")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let section = match crypt_section_name(base_name, profile_section, opts, names) {
        CryptName::Free(name) => name,
        CryptName::Taken(name) => {
            return CryptSection::Refused(format!(
                "the crypt remote name '{name}' is already used by another remote in this file, \
                 and renaming it would point an existing `rclone sync {name}:` somewhere else. \
                 Rename one of the two in AeroFTP and export again"
            ));
        }
    };
    if !requested.is_empty() && requested != section {
        output.push_str(&format!(
            "# WARNING: `{requested}:` in this file is NOT encrypted; use `{section}:` for the \
             encrypted remote. `{requested}` is the clear remote here, and a crypt cannot share \
             its name.\n"
        ));
    }
    // Never trimmed, not even to decide whether there is one: the key is
    // derived from the secret as it is, spaces and all (a salt of spaces
    // left out would select rclone's default salt), so the exported one must
    // be the same.
    let password = opts
        .get("rcloneCryptPassword")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    // Above the header, where rclone keeps it with this section (see
    // `export_rclone`).
    if password.is_none() {
        output.push_str(
            "# password required but unavailable: store the rclone-crypt\n\
             # overlay password on this profile in AeroFTP and re-export,\n\
             # or run `rclone config` on this remote and set `password`.\n",
        );
    }
    output.push_str(&format!("[{}]\n", section));
    output.push_str("type = crypt\n");
    output.push_str(&format!("remote = {}\n", ini_value(&remote)));
    if let Some(pw) = password {
        output.push_str(&format!(
            "password = {}\n",
            obscure_password(pw).unwrap_or_default()
        ));
    }
    if let Some(pw2) = opts
        .get("rcloneCryptPassword2")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        output.push_str(&format!(
            "password2 = {}\n",
            obscure_password(pw2).unwrap_or_default()
        ));
    }
    if let Some(mode) = opts
        .get("rcloneCryptFilenameEncryption")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        output.push_str(&format!("filename_encryption = {}\n", ini_value(mode)));
    }
    if let Some(dir) = opts
        .get("rcloneCryptDirectoryNameEncryption")
        .and_then(|v| v.as_bool())
    {
        output.push_str(&format!(
            "directory_name_encryption = {}\n",
            if dir { "true" } else { "false" }
        ));
    }
    output.push('\n');
    CryptSection::Written
}

/// A profile, or a profile's crypt overlay, that the export refused to write.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RcloneExportSkip {
    /// The AeroFTP profile name, as the operator knows it.
    pub name: String,
    pub reason: String,
}

/// What an export produced: the remotes written, and what was left out.
///
/// A bare count could not tell "nothing to export" from "everything was
/// refused". An export of only Zoho profiles whose region rclone cannot
/// address returned `Ok(0)`, and the CLI printed a success envelope and exited
/// 0, while the adjacent empty-selection case exits 4: the same outcome for the
/// operator, opposite contracts for anything scripted on top.
#[derive(Debug, Clone, Default)]
pub struct RcloneExportOutcome {
    /// Sections written that rclone will load as usable remotes. A crypt
    /// overlay counts as its own remote, because it is one, and so does the
    /// alias that carries a start folder or a pinned bucket.
    pub exported: usize,
    /// Profiles that reached the file, which is what the GUI and the CLI
    /// report against the number selected: one profile can write three
    /// remotes (its own, an alias, a crypt overlay).
    pub profiles: usize,
    pub skipped: Vec<RcloneExportSkip>,
}

impl RcloneExportOutcome {
    fn skip(&mut self, name: &str, reason: &str) {
        self.skipped.push(RcloneExportSkip {
            name: name.to_string(),
            reason: reason.to_string(),
        });
    }
}

/// Highest suffix tried when a name is taken. Reached only by a file with
/// hundreds of profiles sanitizing to one name; refusing beats looping.
const MAX_NAME_SUFFIX: usize = 999;

/// The rclone FTP backend's TLS switches for the mode the connection uses
/// ([`crate::bridge_shared::ftp_tls_mode_for_export`]): `tls` is implicit
/// FTPS and `explicit_tls` is `AUTH TLS`; rclone refuses both at once. It
/// has no "TLS if the server offers it", so that mode requires TLS rather
/// than hand rclone a cleartext session where AeroFTP would have encrypted.
fn rclone_ftp_tls_lines(protocol: &str, options: Option<&serde_json::Value>) -> &'static str {
    // `disable_tls13` travels with either mode, and it is not a workaround for
    // someone else's bug: it is the same decision this client makes for itself,
    // written where rclone can read it.
    //
    // RFC 4217 section 10.2 requires every data connection to resume the SAME
    // TLS session as the control connection, and servers enforce it (vsftpd's
    // `require_ssl_reuse` defaults to on and is usually absent from the config
    // file, so it applies without being written). Under TLS 1.3 a ticket is
    // single-use, so the data connection resumes a DIFFERENT session and the
    // server refuses it. `providers::ftp::make_tls_connector` pins TLS 1.2 for
    // exactly this reason.
    //
    // Measured on 2026-09-19 against the lab vsftpd: rclone with only
    // `explicit_tls` fails the transfer with `426 Failure reading network
    // stream`, and the same rclone with `--ftp-disable-tls13` completes and
    // the bytes land. Exporting the first form hands the user a remote this
    // client knows cannot work against a server that follows the RFC.
    match crate::bridge_shared::ftp_tls_mode_for_export(protocol, options) {
        Some("implicit") => "tls = true\ndisable_tls13 = true\n",
        Some("explicit") | Some("explicit_if_available") => {
            "explicit_tls = true\ndisable_tls13 = true\n"
        }
        _ => "",
    }
}
/// The TLS lines of an FTP remote: the mode ([`rclone_ftp_tls_lines`]) and,
/// when the profile turned certificate verification off, the FTP backend's own
/// `no_check_certificate`. Without it a profile that AeroFTP opens against a
/// shared-hosting certificate issued for another name (measured on 2026-11-06:
/// seven Aruba FTPS profiles) is refused by rclone with `tls: failed to verify
/// certificate`. A cleartext session has no certificate, so nothing is written.
fn push_rclone_ftp_tls(body: &mut String, protocol: &str, options: Option<&serde_json::Value>) {
    let tls = rclone_ftp_tls_lines(protocol, options);
    body.push_str(tls);
    if !tls.is_empty() && profile_verify_cert_off(options) {
        body.push_str("no_check_certificate = true\n");
    }
}

/// The start folder of a WebDAV profile as a path under the URL its remote
/// was written with. The URL already ends in the DAV collection (a Nextcloud
/// `/remote.php/dav/files/<user>/`, a `basePath`), and Nextcloud-derived
/// presets keep that same collection, `{username}` included, as their start
/// folder. Taken verbatim, the alias opened the collection inside itself, with
/// a literal `{username}`, and rclone answered "directory not found"
/// (measured on 2026-11-06 with TAB.DIGITAL and a lab Nextcloud). Empty when
/// the start folder is the collection itself, so no alias is needed.
fn webdav_alias_path(url: &str, initial_path: &str, username: &str) -> String {
    // The URL holds its path percent-encoded (`/team%20docs/`) and the start
    // folder decoded (`/team docs/Photos`), so the URL's is decoded before the
    // two are compared; compared as written, the collection stayed in the
    // alias path.
    let decoded = |p: &str| {
        percent_encoding::percent_decode_str(p)
            .decode_utf8()
            .map(|d| d.into_owned())
            .unwrap_or_else(|_| p.to_string())
    };
    let trim = |p: &str| p.trim_matches('/').to_string();
    let start = trim(&initial_path.replace("{username}", username));
    let url_path = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .split_once('/')
        .map(|(_, path)| trim(&decoded(path)))
        .unwrap_or_default();
    if url_path.is_empty() || start == url_path {
        return if url_path.is_empty() {
            start
        } else {
            String::new()
        };
    }
    match start.strip_prefix(&format!("{url_path}/")) {
        Some(rest) => rest.to_string(),
        None => start,
    }
}

/// An sftp overlay scope saved under the login folder (`/~/projects/vault`)
/// as a path from that folder (`/projects/vault`), the form the alias and the
/// sftp remote use; `None` for a scope that is not under `~`.
fn sftp_login_relative_scope(scope: &str) -> Option<String> {
    let rest = scope.trim_start_matches('/').strip_prefix('~')?;
    (rest.is_empty() || rest.starts_with('/')).then(|| format!("/{}", rest.trim_start_matches('/')))
}

/// The section names of one exported file.
///
/// rclone merges duplicate sections key by key with the last one winning, so a
/// name emitted twice does not produce two remotes: it produces one, made of
/// both. That is how a crypt overlay could vanish into the S3 remote named
/// after it. Two spaces of names feed the same file, the profiles' own and the
/// ones this exporter invents (`<base>-<bucket>` aliases, `<base>-crypt`
/// overlays), and the sanitizer maps distinct profile names onto the same
/// output as well, so uniqueness has to be owned in one place for the whole
/// file rather than checked one collision at a time.
struct RcloneNamespace {
    /// Every profile's own sanitized name, known before the first section is
    /// written so a generated name cannot take one a later profile still needs.
    reserved: HashSet<String>,
    /// Names actually written so far.
    used: HashSet<String>,
}

impl RcloneNamespace {
    fn new(servers: &[RcloneExportServer]) -> Self {
        let reserved = servers
            .iter()
            .map(|s| sanitize_rclone_remote_name(&s.name))
            .filter(|n| !n.is_empty())
            .collect();
        Self {
            reserved,
            used: HashSet::new(),
        }
    }

    /// A profile's own name. It yields only to a section already written, never
    /// to a name this exporter invented.
    fn claim_profile(&mut self, base: &str) -> Option<String> {
        self.claim(base, false)
    }

    /// A name this exporter invents. It also steps aside for every profile
    /// name, including profiles whose section has not been written yet.
    fn claim_generated(&mut self, base: &str) -> Option<String> {
        self.claim(base, true)
    }

    /// Whether `candidate` and a section of this file would be ancestor and
    /// descendant. rclone's config parser (goconfig) reads a key that `[a.b]`
    /// lacks from `[a]`, the section before its last dot, so `[Lab.x]` with no
    /// `pass` logs in to its own host with `[Lab]`'s password (measured on
    /// rclone 1.75.2 against a local FTP listener, 2026-11-06). A name whose
    /// dot-prefix is a section, or will be one (a profile still to write), is
    /// the descendant; a name that is the dot-prefix of a section already
    /// written would become the ancestor. `above_reserved` also counts the
    /// profiles still to write as descendants, for a name that must not push
    /// one aside: an overlay or generated `vault` written before the profile
    /// `vault.team` turned that profile into `vault-team`, which commands
    /// using `vault.team:` no longer address.
    fn dotted_kin(&self, candidate: &str, above_reserved: bool) -> bool {
        let ancestor_taken = candidate.match_indices('.').any(|(i, _)| {
            let prefix = &candidate[..i];
            self.used.contains(prefix) || self.reserved.contains(prefix)
        });
        let below = format!("{candidate}.");
        ancestor_taken
            || self.used.iter().any(|u| u.starts_with(&below))
            || (above_reserved && self.reserved.iter().any(|r| r.starts_with(&below)))
    }

    /// A name the operator wrote by hand: taken exactly, or not at all.
    fn claim_exact(&mut self, name: &str) -> bool {
        if self.used.contains(name) || self.reserved.contains(name) || self.dotted_kin(name, true) {
            return false;
        }
        self.used.insert(name.to_string());
        true
    }

    /// Give a name back. Used when a profile turns out to write no section, so
    /// it cannot push a later profile onto a suffix for nothing.
    fn release(&mut self, name: &str) {
        self.used.remove(name);
    }

    fn claim(&mut self, base: &str, avoid_reserved: bool) -> Option<String> {
        // A descendant gives up its dots: a suffix alone keeps it under the
        // same ancestor (`Lab.x-2` still reads `[Lab]`). The undotted name is
        // one this exporter invents, so it steps aside for every profile name:
        // taken as it was, `Lab.x` became `Lab-x` and the real `Lab-x` moved
        // to `Lab-x-2`, where existing commands no longer find it.
        let undotted;
        let (base, avoid_reserved) = if self.dotted_kin(base, avoid_reserved) {
            undotted = base.replace('.', "-");
            (undotted.as_str(), true)
        } else {
            (base, avoid_reserved)
        };
        let free = |me: &Self, candidate: &str| {
            !me.used.contains(candidate)
                && !(avoid_reserved && me.reserved.contains(candidate))
                && !me.dotted_kin(candidate, avoid_reserved)
        };
        if free(self, base) {
            self.used.insert(base.to_string());
            return Some(base.to_string());
        }
        for suffix in 2..=MAX_NAME_SUFFIX {
            let candidate = format!("{base}-{suffix}");
            if free(self, &candidate) {
                self.used.insert(candidate.clone());
                return Some(candidate);
            }
        }
        None
    }
}

pub fn export_rclone(
    servers: &[RcloneExportServer],
    passwords: &HashMap<String, String>,
    file_path: &Path,
) -> Result<RcloneExportOutcome, String> {
    let mut output = String::new();
    // The version tells a later import which fields this build wrote obscured
    // (see `aeroftp_export_header`).
    output.push_str(&format!(
        "# Generated by AeroFTP {} - https://aeroftp.app\n",
        env!("CARGO_PKG_VERSION")
    ));
    output.push_str(&format!(
        "# Exported: {}\n\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));

    let mut outcome = RcloneExportOutcome::default();
    // Owns the section names of the whole file, real profile names reserved up
    // front so a generated one cannot take a name a later profile still needs.
    let mut names = RcloneNamespace::new(servers);

    for server in servers {
        // INI value safety: clean every free-text field before it is written so
        // a crafted host/username/option can never forge a new [section].
        let server = sanitize_export_server(server);
        let server = &server;
        let proto = server.protocol.as_deref().unwrap_or("ftp");
        let options = server.options.as_ref();
        let password = crate::bridge_shared::export_secret(passwords, &server.id);

        // Sanitize remote name to rclone's own naming rules, not just INI
        // safety: rclone refuses names outside `^[\w.+@ -]+$`, so profiles
        // like "axpbuntu-remote (admin)" previously produced a config
        // rclone could not load at all.
        let sanitized = sanitize_rclone_remote_name(&server.name);
        if sanitized.is_empty() {
            outcome.skip(
                &server.name,
                "profile name has no characters rclone accepts in a remote name",
            );
            continue;
        }

        // rclone's zoho backend interpolates `region` as the TLD in
        // accounts.zoho.{region}. Skip profiles whose AeroFTP slug cannot
        // be expressed that way (Canada's zohocloud.ca, unknown garbage)
        // rather than writing a remote rclone will never reach.
        if proto == "zohoworkdrive" {
            let region = options
                .and_then(|o| o.get("region"))
                .and_then(|v| v.as_str())
                .unwrap_or("us");
            if zoho_region_to_rclone(region).is_none() {
                let reason = format!(
                    "rclone's zoho backend cannot address region '{}'",
                    region.replace(['\n', '\r'], " ")
                );
                output.push_str(&format!(
                    "# skipped Zoho profile '{}': {}\n\n",
                    sanitized, reason
                ));
                outcome.skip(&server.name, &reason);
                continue;
            }
            // rclone's zoho backend cannot list a remote without the
            // privatespace id: it answers F6016 "URL Rule is not configured".
            // The id is normally discovered live before the export, but that
            // discovery returns silently on failed auth, on no network and on
            // timeout, so a profile can reach here without one. Writing the
            // section anyway produces a remote that lists nothing and reports
            // itself as exported, which is worse than not writing it.
            let has_root = options
                .and_then(|o| o.get("root_folder_id"))
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            if !has_root {
                let reason = "Zoho root_folder_id is unknown (discovery unavailable): \
                              rclone would answer F6016 on every listing. Connect the \
                              profile in AeroFTP once, then export again."
                    .to_string();
                output.push_str(&format!(
                    "# skipped Zoho profile '{}': {}\n\n",
                    sanitized, reason
                ));
                outcome.skip(&server.name, &reason);
                continue;
            }
        }

        if let Some(reason) = rclone_required_field_missing(proto, server) {
            output.push_str(&format!(
                "# skipped profile '{}': {}\n\n",
                sanitized, reason
            ));
            outcome.skip(&server.name, &reason);
            continue;
        }

        // The name this section will carry. A real profile name yields only to
        // a section already written, never to a name this exporter invented.
        let remote_name = match names.claim_profile(&sanitized) {
            Some(name) => name,
            None => {
                outcome.skip(&server.name, "no free rclone remote name for this profile");
                continue;
            }
        };

        // The body is built before anything is written, so there is no order of
        // statements in which a `[section]` header can reach the file without
        // the `type =` line that gives it a backend: rclone loads such a section
        // as a remote with no backend at all.
        let mut body = String::new();
        // Guidance for this remote, written ABOVE its `[section]` header:
        // rclone keeps a comment with the section that follows it, so one
        // written inside the body moves under the next remote the first time
        // rclone rewrites the file (measured on rclone v1.75.1).
        let mut notes = String::new();
        // Sections this profile emits after its own (the S3 bucket alias).
        let mut trailing = String::new();
        // What a crypt overlay on this profile must wrap. The S3 arm points it
        // at the bucket alias, since wrapping the raw s3 remote makes rclone
        // look for a bucket named after the overlay path.
        let mut crypt_base = remote_name.clone();
        // The folder of the clear remote that `crypt_base` points at when it
        // is an alias (a pinned bucket, the start folder).
        let mut crypt_base_root: Option<String> = None;
        // Set where `crypt_base_root` is a start folder rather than a bucket.
        let mut crypt_root_is_folder = false;
        // That folder when its alias could not be written: an overlay scope is
        // relative to it, so the crypt wraps the clear remote there.
        let mut crypt_folder: Option<String> = None;
        // The URL a WebDAV remote was written with: its path is already the
        // root rclone lists, so the start-folder alias is relative to it.
        let mut webdav_url: Option<String> = None;

        match proto {
            "ftp" => {
                body.push_str("type = ftp\n");
                body.push_str(&format!("host = {}\n", server.host));
                body.push_str(&format!("port = {}\n", server.port));
                body.push_str(&format!("user = {}\n", server.username));
                // An `ftp` profile may still connect with TLS (`tlsMode`).
                push_rclone_ftp_tls(&mut body, proto, server.options.as_ref());
                if let Some(pw) = password {
                    body.push_str(&format!(
                        "pass = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            "ftps" => {
                body.push_str("type = ftp\n");
                body.push_str(&format!("host = {}\n", server.host));
                body.push_str(&format!("port = {}\n", server.port));
                body.push_str(&format!("user = {}\n", server.username));
                push_rclone_ftp_tls(&mut body, proto, server.options.as_ref());
                if let Some(pw) = password {
                    body.push_str(&format!(
                        "pass = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            "sftp" => {
                body.push_str("type = sftp\n");
                body.push_str(&format!("host = {}\n", server.host));
                body.push_str(&format!("port = {}\n", server.port));
                body.push_str(&format!("user = {}\n", server.username));
                // AeroFTP verifies the server key against ~/.ssh/known_hosts
                // (russh `check_known_hosts`); an rclone remote without this key
                // performs no host-key validation at all and says so on every
                // run. Point it at the same file so the exported remote keeps
                // the trust the profile had (rclone expands the leading `~`).
                body.push_str("known_hosts_file = ~/.ssh/known_hosts\n");
                if let Some(pw) = password.filter(|p| !p.is_empty()) {
                    body.push_str(&format!(
                        "pass = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
                // SFTP key auth: AeroFTP stores the private key as a file
                // path in `options.private_key_path` (+ optional
                // `key_passphrase`). Exporting these key-auth profiles as
                // password-only made rclone fail SSH auth even though
                // `aeroftp-cli connect` succeeded with the key. We emit a
                // `key_file` reference (no private-key bytes are copied
                // into the plaintext config).
                if let Some(opts) = options {
                    if let Some(key_file) = opts
                        .get("private_key_path")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        body.push_str(&format!("key_file = {}\n", key_file));
                    }
                    if let Some(key_pass) = opts
                        .get("key_passphrase")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        // rclone requires key_file_pass obscured, like `pass`.
                        body.push_str(&format!(
                            "key_file_pass = {}\n",
                            obscure_password(key_pass).unwrap_or_default()
                        ));
                    }
                }
            }
            "s3" => {
                body.push_str("type = s3\n");
                // AeroFTP persists empty directories as `<key>/` objects.
                // rclone must recognize them, including the purge root's own
                // marker, instead of constructing a DELETE for `<key>//`.
                body.push_str("directory_markers = true\n");
                let provider_id = server.provider_id.as_deref().unwrap_or("custom-s3");
                // rclone S3 backend providers: see `rclone help backend s3`.
                // Names are case-sensitive; "Other" forces the generic
                // signature path which works for unknown S3-compatible
                // endpoints but skips provider-specific quirks.
                let rclone_provider = match provider_id {
                    "amazon-s3" | "aws-s3" => "AWS",
                    "cloudflare-r2" => "Cloudflare",
                    "digitalocean-spaces" => "DigitalOcean",
                    "wasabi" => "Wasabi",
                    // B2's S3 API uses the generic backend. "Backblaze" is
                    // not a valid rclone S3 provider (native B2 is type=b2).
                    "backblaze" | "backblaze-b2" => "Other",
                    "linode-object-storage" => "Linode",
                    "scaleway" => "Scaleway",
                    "ibm-cos" => "IBMCOS",
                    "storj" => "Storj",
                    "idrive-e2" => "IDrive",
                    "minio" => "Minio",
                    "ionos-s3" => "IONOS",
                    "alibaba-oss" => "Alibaba",
                    "tencent-cos" => "TencentCOS",
                    "qiniu-kodo" => "Qiniu",
                    "google-cloud-storage" => "GCS",
                    // Filebase has no native rclone S3 provider token; the
                    // generic "Other" signature path works with its endpoint.
                    "filebase" => "Other",
                    // Tigris has no rclone provider token either (checked
                    // 2026-11-05); "Other" plus an explicit region and
                    // addressing style, written below, keeps rclone on the
                    // virtual-host path Tigris documents.
                    "tigris" => "Other",
                    _ => "Other",
                };
                body.push_str(&format!("provider = {}\n", rclone_provider));
                body.push_str(&format!("access_key_id = {}\n", server.username));
                if let Some(pw) = password {
                    // S3 `secret_access_key` is NOT a `IsPassword: true`
                    // field in rclone's backend definition: it must be
                    // emitted in plain form. Obscuring it produces an
                    // `AWS4-HMAC-SHA256` signature mismatch on every
                    // request because rclone hashes the obscured string
                    // verbatim instead of reversing it first.
                    body.push_str(&format!("secret_access_key = {}\n", ini_value(pw)));
                }
                let mut pinned_bucket: Option<String> = None;
                let mut endpoint_from_opts: Option<String> = None;
                let mut verify_cert_off = false;
                let mut region_written = false;
                let mut path_style_option: Option<bool> = None;
                if let Some(opts) = options {
                    if let Some(region) = opts.get("region").and_then(|v| v.as_str()) {
                        // A blank region on a Tigris profile is no region: it
                        // gets the `auto` written below, as the preset does.
                        if !(provider_id == "tigris" && region.trim().is_empty()) {
                            body.push_str(&format!("region = {}\n", region));
                            region_written = true;
                        }
                    }
                    // The GUI stores a bool; a normalised profile may carry the
                    // string, as `verify_cert` can.
                    path_style_option = opts
                        .get("pathStyle")
                        .or_else(|| opts.get("path_style"))
                        .and_then(|v| {
                            v.as_bool().or_else(|| {
                                match v.as_str()?.trim().to_ascii_lowercase().as_str() {
                                    "true" => Some(true),
                                    "false" => Some(false),
                                    _ => None,
                                }
                            })
                        });
                    if let Some(endpoint) = opts
                        .get("endpoint")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        endpoint_from_opts = Some(endpoint.to_string());
                    }
                    if let Some(b) = s3_pinned_bucket(options) {
                        pinned_bucket = Some(b.to_string());
                    }
                    verify_cert_off = profile_verify_cert_off(Some(opts));
                }

                // Resolve the effective endpoint in precedence order:
                //   1. explicit `options.endpoint`
                //   2. the profile host field (Storj S3 gateway, Cloudflare R2
                //      via the legacy URL field, generic S3 remotes embed the
                //      endpoint URL directly in the host)
                //   3. the provider-registry preset (Google Cloud Storage,
                //      FileLu S5, ...). Without this last fallback, registry-
                //      default presets exported with no endpoint at all and
                //      rclone silently fell back to the AWS auto-region host
                //      `<bucket>.s3.auto.amazonaws.com`, which is unreachable
                //      (observed on Google S3-interop: `dial 127.0.0.1:443`).
                //      Mirrors the connect path's apply_s3_profile_defaults.
                let resolved_endpoint = endpoint_from_opts
                    .or_else(|| {
                        let h = server.host.trim();
                        if h.is_empty() {
                            None
                        } else {
                            Some(h.to_string())
                        }
                    })
                    .or_else(|| {
                        let mut probe: HashMap<String, String> = HashMap::new();
                        if let Some(opts) = options.and_then(|v| v.as_object()) {
                            for (k, v) in opts {
                                crate::profile_loader::insert_profile_option(&mut probe, k, v);
                            }
                        }
                        // Host already tried above: resolve the preset only.
                        crate::profile_loader::apply_s3_profile_defaults(
                            &mut probe,
                            Some(provider_id),
                            "",
                        )
                    });

                if let Some(endpoint) = resolved_endpoint {
                    let endpoint = endpoint.trim_end_matches('/');
                    let endpoint =
                        if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                            endpoint.to_string()
                        } else {
                            format!("https://{}", endpoint)
                        };
                    body.push_str(&format!("endpoint = {}\n", endpoint));

                    // Local self-signed bridges (Filen Desktop S3 at
                    // https://127.0.0.1:1800) present a certificate without IP
                    // SANs that rclone rejects by default; AeroFTP itself
                    // accepts invalid certs on loopback. Honour an explicit
                    // verify_cert=false from the profile too. The s3 backend has
                    // no such option, hence the `override.` form.
                    if verify_cert_off || endpoint_stays_on_this_machine(&endpoint) {
                        body.push_str(RCLONE_OVERRIDE_NO_CHECK_CERT);
                    }
                }

                // `Other` carries no vendor defaults, and rclone's own default
                // for `force_path_style` is true. Tigris documents virtual-host
                // addressing with region `auto`, so say both explicitly; a
                // profile that stored an addressing choice keeps it.
                if provider_id == "tigris" {
                    if !region_written {
                        body.push_str("region = auto\n");
                    }
                    // rclone cannot reach a bucket with dots in its name over
                    // virtual-host addressing (the wildcard certificate covers
                    // one label), so a pinned dotted bucket defaults to
                    // path-style when no choice was stored.
                    let dotted_bucket = pinned_bucket.as_deref().is_some_and(|b| b.contains('.'));
                    body.push_str(&format!(
                        "force_path_style = {}\n",
                        path_style_option.unwrap_or(dotted_bucket)
                    ));
                }

                // Bucket reconciliation. The s3 backend has no `bucket` config
                // key: it is silently ignored, so a profile-pinned bucket used
                // to round-trip wrong (objects written by AeroFTP at key
                // `<obj>` in bucket `<b>` were only reachable as
                // `<remote>:<b>/<obj>`, while `<remote>:` gave "directory not
                // found"). When the profile pins a bucket we emit an `alias`
                // remote whose path is exactly `<remote>:<bucket>`, so the
                // alias addresses objects bucket-relative, identical to how
                // the AeroFTP S3 client writes keys (drop-in for crypt/sync).
                if let Some(bucket) = pinned_bucket {
                    match write_bucket_alias(
                        &mut names,
                        &mut trailing,
                        &mut notes,
                        &remote_name,
                        &bucket,
                        "bucket",
                    ) {
                        // A crypt overlay must wrap the alias, not the raw s3
                        // remote, and it must wrap the name that was written.
                        Some(alias_name) => {
                            crypt_base = alias_name;
                            crypt_base_root = Some(bucket);
                        }
                        None => crypt_folder = Some(bucket),
                    }
                }
            }
            "webdav" => {
                body.push_str("type = webdav\n");
                // Nextcloud-derived presets (TAB.DIGITAL, FeliCloud, generic
                // Nextcloud) speak the same WebDAV dialect as upstream
                // Nextcloud. Mapping them to vendor=nextcloud lets rclone
                // use the correct chunked-upload + checksum behaviour.
                let vendor = match server.provider_id.as_deref() {
                    Some("nextcloud")
                    | Some("nextcloud-webdav")
                    | Some("tabdigital")
                    | Some("tabdigital-webdav")
                    | Some("felicloud")
                    | Some("felicloud-webdav") => "nextcloud",
                    Some("owncloud") | Some("owncloud-webdav") => "owncloud",
                    _ => "other",
                };
                let base_path = options
                    .and_then(|o| o.get("basePath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Some host strings already carry the scheme (`https://...`)
                // because the GUI persists the full URL as the host on
                // certain WebDAV presets. In that case we MUST NOT prepend
                // another scheme or the URL becomes `https://https://...`
                // and rclone refuses to dial.
                let host = server.host.trim_end_matches('/');
                let url = if host.starts_with("http://") || host.starts_with("https://") {
                    format!("{}{}", host, base_path)
                } else {
                    let scheme = webdav_export_scheme(host, server.port, options);
                    if webdav_is_local_bridge(host, server.port) {
                        notes.push_str(&format!(
                            "# '{}': local bridge on plain HTTP, its default; if TLS is on in the bridge app, make the url https:// (self-signed: add override.no_check_certificate = true)\n",
                            remote_name
                        ));
                    }
                    // Only the scheme's own default port goes without saying:
                    // HTTP on 443 written as `http://host` dialled 80.
                    let default_port = if scheme == "https" { 443 } else { 80 };
                    let port_str = if server.port == default_port || server.port == 0 {
                        String::new()
                    } else {
                        format!(":{}", server.port)
                    };
                    format!("{}://{}{}{}", scheme, host, port_str, base_path)
                };
                // Resolve {username} template that Nextcloud-derived
                // presets store verbatim in the URL or initial path
                // (TAB.DIGITAL, FeliCloud).
                let url = url.replace("{username}", &server.username);
                // rclone's `nextcloud`/`owncloud` vendors do not auto-discover
                // the DAV collection root the way AeroFTP does at connect time
                // (PROPFIND probe -> `/remote.php/dav/files/<user>/`). They
                // require the full collection URL up front, otherwise the
                // exported remote cannot list or transfer. AeroFTP profiles
                // usually persist a bare host (empty basePath), so synthesise
                // the path here when it is missing. Generic WebDAV (vendor
                // `other`: Koofr, SharePoint, Fastmail) has no such convention
                // and is left untouched. Any explicit `/remote.php/` segment
                // already present is honoured verbatim.
                let url = if matches!(vendor, "nextcloud" | "owncloud")
                    && !url.contains("/remote.php/")
                {
                    format!(
                        "{}/remote.php/dav/files/{}/",
                        url.trim_end_matches('/'),
                        server.username
                    )
                } else {
                    url
                };
                body.push_str(&format!("url = {}\n", url));
                body.push_str(&format!("vendor = {}\n", vendor));
                body.push_str(&format!("user = {}\n", server.username));
                if let Some(pw) = password {
                    body.push_str(&format!(
                        "pass = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
                if profile_verify_cert_off(options) {
                    body.push_str(RCLONE_OVERRIDE_NO_CHECK_CERT);
                }
                webdav_url = Some(url);
            }
            "googledrive" => {
                body.push_str("type = drive\n");
                push_oauth_credentials(&mut body, &mut notes, &remote_name, options);
            }
            "dropbox" => {
                body.push_str("type = dropbox\n");
                push_oauth_credentials(&mut body, &mut notes, &remote_name, options);
            }
            "onedrive" => {
                body.push_str("type = onedrive\n");
                // rclone's `onedrive` backend needs `drive_id` + `drive_type`
                // (it fails at use with "unable to get drive_id and drive_type"),
                // even though the schema does not mark them Required. AeroFTP runs
                // on the `/me/drive` shortcut and captures both at connect time
                // into the vault; the bridge injects them into options here. If a
                // profile was never connected after this fix, they are absent and
                // the user must run `rclone config reconnect` / reconnect once in
                // AeroFTP to populate them. `region` is emitted when captured.
                if let Some(opts) = options {
                    if let Some(region) = opts
                        .get("region")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        body.push_str(&format!("region = {}\n", region));
                    }
                    if let Some(drive_id) = opts
                        .get("drive_id")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        body.push_str(&format!("drive_id = {}\n", drive_id));
                    }
                    if let Some(drive_type) = opts
                        .get("drive_type")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        body.push_str(&format!("drive_type = {}\n", drive_type));
                    }
                }
                push_oauth_credentials(&mut body, &mut notes, &remote_name, options);
            }
            "mega" => {
                body.push_str("type = mega\n");
                body.push_str(&format!("user = {}\n", server.username));
                if let Some(pw) = password {
                    body.push_str(&format!(
                        "pass = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            "internxt" => {
                // rclone's `internxt` backend signs in with the account email
                // and password (`pass` is an `IsPassword` field, so obscured),
                // and also needs the decrypted `mnemonic`, which its own login
                // stores in the config. AeroFTP keeps the mnemonic in memory
                // only, so the remote is written without it and rclone derives
                // it on its own sign-in. Until then rclone refuses the remote
                // with "mnemonic is required" (rclone v1.75.1).
                body.push_str("type = internxt\n");
                body.push_str(&format!("email = {}\n", server.username));
                match password.filter(|pw| !pw.is_empty()) {
                    Some(pw) => {
                        body.push_str(&format!(
                            "pass = {}\n",
                            obscure_password(pw).unwrap_or_default()
                        ));
                        notes.push_str(&format!(
                            "# Internxt: run `rclone config reconnect {}` once before use.\n\
                             # rclone signs in with the email and password below and stores\n\
                             # the encryption mnemonic it needs, which AeroFTP does not keep\n\
                             # on disk.\n",
                            rclone_shell_arg(&remote_name, true)
                        ));
                    }
                    // Exported without credentials (the GUI "Include
                    // credentials" switch off). A reconnect would sign in with
                    // an empty password, since rclone does not ask for a
                    // missing one; `update --all` asks for it (not echoed) and
                    // then signs in (checked on rclone v1.75.1).
                    None => {
                        notes.push_str(&format!(
                            "# Internxt: no password was exported for this remote. Run\n\
                             # `rclone config update {} --all`: it asks for the password\n\
                             # without echoing it, then signs in and stores the encryption\n\
                             # mnemonic rclone needs.\n",
                            rclone_shell_arg(&remote_name, false)
                        ));
                    }
                }
                notes.push_str(
                    "# Internxt lets rclone sign in only on plans that include rclone\n\
                     # access: on other plans, the free one included, the sign-in stops\n\
                     # with 402 \"rclone access not allowed for this user tier\".\n",
                );
            }
            "proton" => {
                // Reached only by a native profile with a saved session:
                // `rclone_required_field_missing` turns the others away. The
                // four `client_*` lines are rclone's cached login, which it
                // uses before any password login. No password is written:
                // AeroFTP never holds the account password.
                body.push_str("type = protondrive\n");
                if !server.username.is_empty() {
                    body.push_str(&format!("username = {}\n", ini_value(&server.username)));
                }
                if let Some(lines) = proton_session_of(server) {
                    for (key, value) in lines {
                        body.push_str(&format!("{key} = {}\n", ini_value(&value)));
                    }
                }
                notes.push_str(
                    "# Proton Drive: this remote shares AeroFTP's session. Proton rotates\n\
                     # the refresh token, so the first of the two to refresh keeps it. Add\n\
                     # the account password with `rclone config` so rclone can sign in again\n\
                     # on its own.\n",
                );
            }
            "filen" => {
                // rclone's `filen` backend marks `email`, `password` AND
                // `api_key` all Required, and obtains the api_key only via the
                // Filen CLI `export-api-key` command: it does NOT derive it from
                // email + password. So a remote without an api_key is unusable
                // (it fails at first use with "failed to reveal api key: input
                // too short"). AeroFTP keeps the optional Filen CLI API key in
                // the vault under `filen_api_key_<id>` (issue #230); the export
                // path injects it into `options.filen_api_key`. When it is
                // present we emit a usable remote; when it is absent we emit a
                // commented scaffold telling the user to add the api_key rather
                // than a broken `type = filen` block.
                body.push_str("type = filen\n");
                body.push_str(&format!("email = {}\n", server.username));
                if let Some(pw) = password.filter(|p| !p.is_empty()) {
                    body.push_str(&format!(
                        "password = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
                let api_key = options
                    .and_then(|o| o.get("filen_api_key"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                if let Some(api_key) = api_key {
                    // The api_key is an rclone `IsPassword` field, so it is
                    // emitted obscured like `password`.
                    body.push_str(&format!(
                        "api_key = {}\n",
                        obscure_password(api_key).unwrap_or_default()
                    ));
                } else {
                    // No api_key available: rclone's filen backend Requires it
                    // and cannot derive it from the password, so the remote is
                    // incomplete. Emit a guidance comment (mirroring the OAuth
                    // "reconnect" path) instead of a silently broken remote.
                    notes.push_str(
                        "# api_key required but unavailable: rclone's filen backend\n\
                         # cannot derive it from your password. Get one with the Filen\n\
                         # CLI `export-api-key` command, obscure it (`rclone obscure`),\n\
                         # and add `api_key = <obscured>` to the section below, or set a\n\
                         # `Filen CLI API Key` on this profile in AeroFTP and re-export.\n",
                    );
                }
            }
            "box" => {
                body.push_str("type = box\n");
                push_oauth_credentials(&mut body, &mut notes, &remote_name, options);
            }
            "pcloud" => {
                body.push_str("type = pcloud\n");
                // rclone's pcloud `hostname` must be a real API host:
                // api.pcloud.com (US, the default) or eapi.pcloud.com (EU).
                // AeroFTP stores the region in options.region ("us"/"eu") or, on
                // OAuth profiles, only as a display label in `host`
                // ("pCloud (US)"/"pCloud (EU)"), never a hostname. Emitting that
                // label verbatim produced an unusable remote. Map it: emit the
                // EU host only for EU; omit for US (rclone defaults to it).
                let is_eu = options
                    .and_then(|o| o.get("region"))
                    .and_then(|v| v.as_str())
                    .map(|r| r.eq_ignore_ascii_case("eu"))
                    .unwrap_or_else(|| {
                        let h = server.host.to_ascii_lowercase();
                        h.contains("eu") || h.contains("eapi")
                    });
                if is_eu {
                    body.push_str("hostname = eapi.pcloud.com\n");
                }
                push_oauth_credentials(&mut body, &mut notes, &remote_name, options);
            }
            "azure" => {
                body.push_str("type = azureblob\n");
                body.push_str(&format!("account = {}\n", server.username));
                if let Some(pw) = password {
                    // Azure Blob `key` is the storage account access key
                    // (base64). rclone's azureblob backend does NOT mark
                    // this field as `IsPassword: true`, so it must be
                    // emitted plain (same reasoning as S3 secret_access_key).
                    body.push_str(&format!("key = {}\n", ini_value(pw)));
                }
                if let Some(opts) = options {
                    if let Some(container) = opts.get("bucket").and_then(|v| v.as_str()) {
                        // rclone's azureblob backend has no `container` key and
                        // ignores it; it stays for this importer, and the alias
                        // below is what rclone opens.
                        body.push_str(&format!("container = {}\n", container));
                    }
                }
                // Containers are addressed like S3 buckets (`remote:container`).
                if let Some(container) = options
                    .and_then(|o| o.get("bucket"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .map(str::to_string)
                {
                    match write_bucket_alias(
                        &mut names,
                        &mut trailing,
                        &mut notes,
                        &remote_name,
                        &container,
                        "container",
                    ) {
                        Some(alias_name) => {
                            crypt_base = alias_name;
                            crypt_base_root = Some(container);
                        }
                        None => crypt_folder = Some(container),
                    }
                }
            }
            "swift" => {
                body.push_str("type = swift\n");
                body.push_str(&format!("user = {}\n", server.username));
                if let Some(pw) = password {
                    // Swift `key` is the API key / OS_PASSWORD. rclone's swift
                    // backend does NOT mark this field as `IsPassword: true`, so
                    // it is used verbatim and must be emitted plain (same
                    // reasoning as S3 secret_access_key and Azure key). Emitting
                    // an obscured value makes rclone send the obscured string as
                    // the password, which fails authentication.
                    body.push_str(&format!("key = {}\n", ini_value(pw)));
                }
                let blomp = swift_is_blomp(server);
                if let Some(auth) = swift_auth_url(server) {
                    let versioned = swift_auth_url_has_version(&auth);
                    // AeroFTP finds the Keystone version by probing the auth
                    // root; rclone does not, and refuses an unversioned URL
                    // ("Can't find AuthVersion in AuthUrl"). Blomp is Keystone
                    // v2, so its root gets the version AeroFTP ends up dialing.
                    let auth = if blomp && !versioned {
                        format!("{}/v2.0", auth.trim_end_matches('/'))
                    } else {
                        auth
                    };
                    body.push_str(&format!("auth = {}\n", auth));
                    if !blomp && !versioned {
                        notes.push_str(&format!(
                            "# '{}': the auth URL names no Keystone version; set `auth_version` (1, 2 or 3) for rclone\n",
                            remote_name
                        ));
                    }
                }
                if profile_verify_cert_off(options) {
                    body.push_str(RCLONE_OVERRIDE_NO_CHECK_CERT);
                }
                let tenant = options
                    .and_then(|o| o.get("tenant"))
                    .and_then(|v| v.as_str())
                    .filter(|t| !t.trim().is_empty());
                // Blomp's Keystone v2 tenant is fixed (`auth_keystone_v2`).
                if let Some(tenant) = tenant.or(blomp.then_some("storage")) {
                    body.push_str(&format!("tenant = {}\n", tenant));
                }
                if let Some(opts) = options {
                    if let Some(region) = opts.get("region").and_then(|v| v.as_str()) {
                        body.push_str(&format!("region = {}\n", region));
                    }
                    if let Some(container) = opts.get("bucket").and_then(|v| v.as_str()) {
                        // rclone's swift backend has no `container` key either.
                        body.push_str(&format!("container = {}\n", container));
                    }
                }
                // Blomp lets no one list its containers (403 by design) and
                // gives each account one, named after the login, which AeroFTP
                // opens on its own (`discover_container`): that one is pinned.
                // Swift allows spaces in a container name, so the name is
                // kept as saved: trimmed, ` media` was exported as `media`.
                let container = options
                    .and_then(|o| o.get("bucket"))
                    .and_then(|v| v.as_str())
                    .filter(|c| !c.trim().is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        (blomp && !server.username.trim().is_empty())
                            .then(|| server.username.trim().to_string())
                    });
                if let Some(container) = container {
                    match write_bucket_alias(
                        &mut names,
                        &mut trailing,
                        &mut notes,
                        &remote_name,
                        &container,
                        "container",
                    ) {
                        Some(alias_name) => {
                            crypt_base = alias_name;
                            crypt_base_root = Some(container);
                        }
                        None => crypt_folder = Some(container),
                    }
                }
            }
            "yandexdisk" => {
                // rclone's backend type is `yandex` (not `yandexdisk`); the old
                // value produced a config rclone refused to load.
                body.push_str("type = yandex\n");
                push_oauth_credentials(&mut body, &mut notes, &remote_name, options);
            }
            "koofr" => {
                body.push_str("type = koofr\n");
                body.push_str(&format!("endpoint = https://{}\n", server.host));
                body.push_str(&format!("user = {}\n", server.username));
                if let Some(pw) = password {
                    body.push_str(&format!(
                        "password = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            "jottacloud" => {
                body.push_str("type = jottacloud\n");
                // The password slot carries the Jotta OIDC refresh blob
                // (`{refresh_token, token_endpoint, username}`), the same shape
                // AeroFTP persists under `jottacloud_refresh_<id>`. rclone
                // accepts a token with only a refresh_token: it has no
                // access_token and a zero expiry, so rclone refreshes on first
                // use against the OIDC endpoint and authenticates without an
                // interactive re-login. Device and mountpoint are rediscovered
                // by rclone at runtime.
                if let Some(blob) = password {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(blob) {
                        let refresh = v
                            .get("refresh_token")
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        let endpoint = v
                            .get("token_endpoint")
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        let user = v
                            .get("username")
                            .and_then(|x| x.as_str())
                            .filter(|s| !s.is_empty())
                            .unwrap_or(&server.username);
                        if !user.is_empty() {
                            body.push_str(&format!("user = {}\n", ini_value(user)));
                        }
                        if !refresh.is_empty() {
                            // rclone's jottacloud backend expects a full
                            // oauth2.Token. A bare `{"refresh_token":...}` is
                            // reported as "no refresh token", and an EMPTY
                            // access_token makes rclone discard the token the
                            // same way. Emit a non-empty placeholder access
                            // token with an already-past expiry so rclone treats
                            // it as expired-but-refreshable and refreshes
                            // against the OIDC endpoint on first use.
                            let token = serde_json::json!({
                                "access_token": "expired",
                                "token_type": "Bearer",
                                "refresh_token": refresh,
                                "expiry": "2000-01-01T00:00:00Z",
                            });
                            body.push_str(&format!("token = {}\n", token));
                        }
                        if !endpoint.is_empty() {
                            body.push_str(&format!("token_endpoint = {}\n", ini_value(endpoint)));
                        }
                        // rclone refuses a jottacloud remote without a known
                        // config schema version ("outdated config - please
                        // reconfigure this backend"). Version 1 is the current
                        // schema for the standard auth flow.
                        body.push_str("configVersion = 1\n");
                    }
                }
            }
            "opendrive" => {
                body.push_str("type = opendrive\n");
                body.push_str(&format!("username = {}\n", server.username));
                if let Some(pw) = password {
                    body.push_str(&format!(
                        "password = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            "zohoworkdrive" => {
                // rclone's backend type is `zoho`. Region uses TLD slugs
                // (`com` for US / Global, `com.au` for Australia). AeroFTP
                // stores `us` / `au` (and the other data-centre slugs that
                // match rclone already). Unmappable regions were skipped
                // before the section header; unwrap is then a mapped TLD.
                body.push_str("type = zoho\n");
                let region = options
                    .and_then(|o| o.get("region"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("us");
                let rclone_region =
                    zoho_region_to_rclone(region).expect("zohoworkdrive region preflighted");
                body.push_str(&format!("region = {}\n", ini_value(&rclone_region)));
                if let Some(folder) = options
                    .and_then(|o| o.get("root_folder_id"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    body.push_str(&format!("root_folder_id = {}\n", ini_value(folder)));
                }
                push_oauth_credentials_inner(
                    &mut body,
                    &mut notes,
                    &remote_name,
                    options,
                    Some("Zoho-oauthtoken"),
                );
            }
            "backblaze" => {
                // AeroFTP's native Backblaze protocol maps to rclone's `b2`
                // backend: `account` (the key ID) + `key` (the application key).
                // `key` is NOT an rclone IsPassword field, so it is emitted plain
                // (same as S3 secret_access_key / Swift key). The bucket is part
                // of the remote path in rclone (`remote:bucket`), not a config
                // key, so a pinned one travels as an alias, like S3's.
                body.push_str("type = b2\n");
                body.push_str(&format!("account = {}\n", server.username));
                if let Some(pw) = password.filter(|p| !p.is_empty()) {
                    // F-01: strip CR/LF like the other plain-secret sinks
                    // (s3 secret_access_key, azure/swift key) so a crafted
                    // application key can't forge a second [section].
                    body.push_str(&format!("key = {}\n", ini_value(pw)));
                }
                if let Some(bucket) = options
                    .and_then(|o| o.get("bucket"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|b| !b.is_empty())
                {
                    match write_bucket_alias(
                        &mut names,
                        &mut trailing,
                        &mut notes,
                        &remote_name,
                        bucket,
                        "bucket",
                    ) {
                        Some(alias_name) => {
                            crypt_base = alias_name;
                            crypt_base_root = Some(bucket.to_string());
                        }
                        None => crypt_folder = Some(bucket.to_string()),
                    }
                }
            }
            // Drime, Cloudinary and ImageKit take the same secrets AeroFTP
            // signs with. rclone v1.75.1 marks none of these fields
            // `IsPassword`, so they are written plain, CR/LF stripped like the
            // S3 and B2 keys; obscuring them would hand rclone a key it sends
            // as is and the service refuses. FileLu is not among them: see
            // `bridge_shared::bridge_export_refusal`.
            "drime" => {
                // The API token AeroFTP sends as a Bearer token. rclone reads
                // the default workspace when `workspace_id` is left out, the
                // same one AeroFTP addresses as `workspaceId=0`.
                body.push_str("type = drime\n");
                if let Some(pw) = password.filter(|p| !p.is_empty()) {
                    body.push_str(&format!("access_token = {}\n", ini_value(pw)));
                }
            }
            "cloudinary" => {
                body.push_str("type = cloudinary\n");
                if let Some(cloud_name) = cloudinary_cloud_name(server) {
                    body.push_str(&format!("cloud_name = {}\n", cloud_name));
                }
                body.push_str(&format!("api_key = {}\n", server.username.trim()));
                if let Some(pw) = password.filter(|p| !p.is_empty()) {
                    body.push_str(&format!("api_secret = {}\n", ini_value(pw)));
                }
            }
            "imagekit" => {
                body.push_str("type = imagekit\n");
                body.push_str(&format!(
                    "endpoint = {}\n",
                    imagekit_rclone_endpoint(&server.username)
                ));
                if let Some(public_key) = imagekit_public_key(options) {
                    body.push_str(&format!("public_key = {}\n", public_key));
                }
                if let Some(pw) = password.filter(|p| !p.is_empty()) {
                    body.push_str(&format!("private_key = {}\n", ini_value(pw)));
                }
            }
            // Protocols without rclone equivalent: skip
            _ => {
                // Nothing was written, so the name goes back: a profile that
                // produced no section must not push a later one to a suffix.
                names.release(&remote_name);
                outcome.skip(
                    &server.name,
                    &format!("protocol '{}' has no rclone backend", proto),
                );
                continue;
            }
        }

        // A path-rooted profile (sftp/ftp/webdav) that starts in a sub-folder:
        // rclone has no "start here" key for these backends, so the folder is
        // carried as an `alias` remote, exactly like a pinned S3 bucket. The
        // alias name is generated, so it yields to every real profile name.
        if matches!(proto, "sftp" | "ftp" | "ftps" | "webdav") {
            // `/` is the root only for sftp, whose remote opens the login
            // folder; ftp and webdav remotes already open at `/`.
            if let Some(path) = options
                .and_then(|o| o.get("initial_path"))
                .and_then(|v| v.as_str())
                .filter(|p| !p.is_empty() && (*p != "/" || proto == "sftp"))
            {
                // sftp addresses an absolute folder as `remote:/abs`; ftp and
                // webdav paths are relative to the account/URL root.
                let alias_path = if proto == "sftp" {
                    // `~/docs` is under the login folder, which rclone's
                    // `nas:docs` opens (rclone takes `~` literally); `~` alone
                    // is the login folder the remote already opens.
                    match path.strip_prefix('~') {
                        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                            rest.trim_start_matches('/').to_string()
                        }
                        _ => path.to_string(),
                    }
                } else if let Some(url) = webdav_url.as_deref() {
                    webdav_alias_path(url, path, &server.username)
                } else {
                    path.trim_start_matches('/').to_string()
                };
                // rclone reads a config value without the spaces at its ends
                // (and a quoted one with its quotes), so `remote = nas:/data `
                // would open `/data`: a folder whose name ends in a space
                // cannot be written there, and the alias is left out. An empty
                // alias path is the collection the WebDAV URL already opens.
                if alias_path.is_empty() {
                    // sftp's `~`: the login folder the remote already opens,
                    // where a crypt scope is relative too.
                    if proto == "sftp" {
                        crypt_folder = Some(String::new());
                    }
                } else if path.ends_with(char::is_whitespace) {
                    notes.push_str(&format!(
                        "# start-folder alias for '{}' omitted: rclone's config cannot hold '{}', whose name ends in a space\n",
                        server.name.replace(['\n', '\r'], " "),
                        path.replace(['\n', '\r'], " ")
                    ));
                    crypt_folder = Some(alias_path);
                } else {
                    let desired = sanitize_rclone_remote_name(&format!("{remote_name}-path"));
                    match names.claim_generated(&desired) {
                        Some(alias_name) if !alias_name.is_empty() && alias_name != remote_name => {
                            if alias_name != desired {
                                trailing.push_str(&format!(
                                    "\n# renamed from '{}': that name is taken by another remote\n",
                                    desired
                                ));
                            }
                            trailing.push_str(&format!(
                            "\n# '{}' opens in '{}' in AeroFTP; address that folder as `{}:`.\n",
                            server.name.replace(['\n', '\r'], " "),
                            alias_path,
                            alias_name
                        ));
                            trailing.push_str(&format!("[{}]\n", alias_name));
                            trailing.push_str("type = alias\n");
                            trailing
                                .push_str(&format!("remote = {}:{}\n", remote_name, alias_path));
                            // A crypt overlay scoped by path must wrap the alias,
                            // so `rcloneCryptOverlayScope = /vault` resolves under
                            // the start folder, not under the account root. An
                            // imported `rcloneCryptRemote` is taken relative to
                            // this folder (`crypt_export_remote_target`).
                            crypt_base = alias_name;
                            crypt_base_root = Some(alias_path.to_string());
                            crypt_root_is_folder = true;
                        }
                        _ => {
                            notes.push_str(&format!(
                            "# start-folder alias for '{}' omitted: no free remote name near '{}'\n",
                            server.name.replace(['\n', '\r'], " "),
                            desired
                        ));
                            crypt_folder = Some(alias_path);
                        }
                    }
                }
            }
        }

        // Rclone Crypt is a second remote wrapping this one. The overlay
        // path is always at or below the server's remote path, so
        // `remote = <base>:<path>` is derived rather than guessed. It is built
        // before this remote is written so that a refusal can be noted above
        // this remote's header, where it stays when rclone rewrites the file;
        // the names it claims come after this remote's in either order.
        // The app saves a WebDAV overlay scope as a path on the server, while
        // rclone's webdav remote, and the alias over it, start at the URL's
        // collection: the scope is taken in that form, or the collection
        // would be written twice (`dav-path:/remote.php/dav/files/alice/docs`).
        let scoped_crypt_options = match (proto, webdav_url.as_deref(), options) {
            ("webdav", Some(url), Some(o)) => o
                .get("rcloneCryptOverlayScope")
                .and_then(|v| v.as_str())
                .map(|scope| {
                    let mut o = o.clone();
                    o["rcloneCryptOverlayScope"] = serde_json::Value::String(format!(
                        "/{}",
                        webdav_alias_path(url, scope, &server.username)
                    ));
                    o
                }),
            // Under the login folder the app saves the scope with the `~` it
            // read as a folder (`resolveOverlayScope` turns `~/projects` into
            // `/~/projects`), while the alias holds `projects`: taken as
            // written, the crypt opened `/~/projects` inside the start folder.
            ("sftp", _, Some(o))
                if options
                    .and_then(|o| o.get("initial_path"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|start| start.starts_with('~')) =>
            {
                o.get("rcloneCryptOverlayScope")
                    .and_then(|v| v.as_str())
                    .and_then(sftp_login_relative_scope)
                    .map(|scope| {
                        let mut o = o.clone();
                        o["rcloneCryptOverlayScope"] = serde_json::Value::String(scope);
                        o
                    })
            }
            _ => None,
        };
        let mut crypt_section = String::new();
        let crypt = append_crypt_remote_section(
            &mut crypt_section,
            &crypt_base,
            &remote_name,
            crypt_base_root.as_deref().map(|root| {
                if crypt_root_is_folder {
                    AliasRoot::Folder(root)
                } else {
                    AliasRoot::Bucket(root)
                }
            }),
            crypt_folder.as_deref(),
            scoped_crypt_options.as_ref().or(options),
            &mut names,
        );
        if let CryptSection::Refused(reason) = &crypt {
            notes.push_str(&format!(
                "# crypt overlay for '{}' not written: {}\n",
                crypt_base, reason
            ));
        }

        // Header and body together, never one without the other.
        if remote_name != server.name {
            tracing::warn!(
                "[rclone export] profile '{}' exported as '{}' to satisfy rclone remote-name rules",
                server.name,
                remote_name
            );
            output.push_str(&format!(
                "# renamed from AeroFTP profile \"{}\" (rclone remote-name rules)\n",
                server.name.replace(['\n', '\r'], " ")
            ));
        }
        output.push_str(&notes);
        output.push_str(&format!("[{}]\n", remote_name));
        output.push_str(&body);
        output.push_str(&trailing);
        output.push('\n');
        outcome.profiles += 1;
        outcome.exported += 1 + trailing.matches("\ntype = alias\n").count();

        output.push_str(&crypt_section);
        match crypt {
            CryptSection::Written => outcome.exported += 1,
            CryptSection::None => {}
            CryptSection::Refused(reason) => outcome.skip(&server.name, &reason),
        }
    }

    // Atomic write + secure permissions, through the same helper the other
    // bridge exporters use: its temp file is O_EXCL/0600 (so a symlink planted
    // at a predictable sibling path cannot capture the secrets) and it unlinks
    // itself on any early return instead of leaving a partial cleartext file.
    crate::bridge_shared::atomic_write_600(file_path, output.as_bytes())
        .map_err(|e| format!("Write rclone.conf: {}", e))?;

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which OAuth app stays stored does not change from run to run.
    #[test]
    fn the_secrets_follow_the_order_of_the_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        let remotes: String = (0..8)
            .map(|i| format!("[r{i}]\ntype = drive\nclient_id = app-{i}\nclient_secret = s\n\n"))
            .collect();
        std::fs::write(&path, remotes).unwrap();
        let result = import_rclone(&path).unwrap();
        let apps: Vec<_> = result
            .secrets_in_server_order()
            .into_iter()
            .map(|(_, secrets)| secrets.oauth_client_id.clone().unwrap())
            .collect();
        let expected: Vec<_> = (0..8).map(|i| format!("app-{i}")).collect();
        assert_eq!(apps, expected);

        // A token refreshes only with its own app: its remote chooses first.
        std::fs::write(
            &path,
            "[a]\ntype = drive\nclient_id = no-token\nclient_secret = s\n\n\
             [b]\ntype = drive\nclient_id = with-token\nclient_secret = s\n\
             token = {\"access_token\":\"x\",\"refresh_token\":\"y\"}\n",
        )
        .unwrap();
        let result = import_rclone(&path).unwrap();
        let first = result.secrets_in_server_order()[0].1;
        assert_eq!(first.oauth_client_id.as_deref(), Some("with-token"));
    }

    #[test]
    fn refreshable_tokens_precede_access_only_and_app_only_remotes_stably() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        let remotes = [
            ("app-only", ""),
            ("access-only", r#"{"access_token":"a"}"#),
            (
                "empty-refresh",
                r#"{"access_token":"a","refresh_token":"  "}"#,
            ),
            (
                "refresh-first",
                r#"{"access_token":"a","refresh_token":"r"}"#,
            ),
            (
                "refresh-second",
                r#"{"access_token":"b","refresh_token":"s"}"#,
            ),
        ]
        .into_iter()
        .map(|(name, token)| {
            format!(
                "[{name}]\ntype = drive\nclient_id = {name}\nclient_secret = s\n{}\n",
                if token.is_empty() {
                    String::new()
                } else {
                    format!("token = {token}\n")
                }
            )
        })
        .collect::<String>();
        std::fs::write(&path, remotes).unwrap();
        let result = import_rclone(&path).unwrap();
        let apps: Vec<_> = result
            .secrets_in_server_order()
            .iter()
            .map(|(_, secret)| secret.oauth_client_id.as_deref().unwrap())
            .collect();
        assert_eq!(
            apps,
            [
                "refresh-first",
                "refresh-second",
                "access-only",
                "empty-refresh",
                "app-only"
            ]
        );
    }

    /// An app with no token yet is still kept: a later sign-in on this device needs it.
    #[test]
    fn an_oauth_remote_without_a_token_keeps_its_app() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        std::fs::write(
            &path,
            "[app-only]\ntype = drive\nclient_id = my-app\nclient_secret = my-secret\n\n\
             [plain]\ntype = sftp\nhost = h.example.invalid\nuser = u\nclient_id = not-oauth\n",
        )
        .unwrap();
        let result = import_rclone(&path).unwrap();
        let id = result_app(&result, "app-only");
        let secrets = result.provider_secrets.get(&id).expect("the app");
        assert_eq!(secrets.oauth_client_id.as_deref(), Some("my-app"));
        assert_eq!(secrets.oauth_client_secret.as_deref(), Some("my-secret"));
        assert!(secrets.oauth.is_none());
        let plain = result_app(&result, "plain");
        assert!(!result.provider_secrets.contains_key(&plain));
    }

    fn result_app(result: &RcloneImportResult, name: &str) -> String {
        let server = result.servers.iter().find(|s| s.name == name).expect(name);
        server.id.clone()
    }

    #[test]
    fn swift_import_keeps_the_auth_url_consumed_by_the_provider() {
        let result = parse_rclone_conf(
            "[swift-test]\ntype = swift\nauth = http://127.0.0.1:8080/identity/v3/auth/tokens\nuser = synthetic\nkey = synthetic\n",
        );
        assert_eq!(result.len(), 1);
        let profile = map_remote("swift-test", &result["swift-test"], None).unwrap();
        assert_eq!(
            profile.host,
            "http://127.0.0.1:8080/identity/v3/auth/tokens"
        );
        assert_eq!(profile.port, 8080);
    }

    /// Write `conf` to a uniquely-named temp file and return its path.
    fn tmp_write(conf: &str, name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, conf).expect("write temp conf");
        path
    }

    /// Minimal exportable profile, so the namespace tests read as the case
    /// they are about instead of as a wall of struct fields.
    fn export_server(
        name: &str,
        protocol: &str,
        options: Option<serde_json::Value>,
    ) -> RcloneExportServer {
        RcloneExportServer {
            id: name.to_string(),
            name: name.to_string(),
            host: "host.example".to_string(),
            port: 21,
            username: "user".to_string(),
            protocol: Some(protocol.to_string()),
            options,
            provider_id: None,
        }
    }

    fn export_to_string(
        servers: &[RcloneExportServer],
        tag: &str,
    ) -> (RcloneExportOutcome, String) {
        export_with_passwords(servers, &[], tag)
    }

    /// C-05. A crypt overlay names its section `<base>-crypt`, and nothing
    /// stopped that from being the name of a real profile. rclone does not
    /// reject a duplicate section: it merges the two key by key, last one
    /// wins, so the crypt remote the operator believed they exported is not
    /// there and `rclone sync` to that name writes in the clear. Reproduced on
    /// rclone 1.74.0.
    #[test]
    fn export_rclone_never_writes_one_section_name_twice() {
        let servers = vec![
            export_server(
                "minio",
                "ftp",
                Some(serde_json::json!({
                    "rcloneCryptEnabled": true,
                    "rcloneCryptPassword": "topsecret",
                })),
            ),
            export_server("minio-crypt", "ftp", None),
        ];
        let (outcome, conf) = export_to_string(&servers, "dup-names");

        assert_eq!(
            conf.matches("[minio-crypt]\n").count(),
            1,
            "exactly one section may carry a given name:\n{conf}"
        );
        // The real profile keeps the name it was given. The generated overlay
        // is the one that moves, because it is this exporter's invention.
        let crypt_at = conf
            .find("[minio-crypt-2]")
            .unwrap_or_else(|| panic!("the overlay must take a free name:\n{conf}"));
        assert!(
            conf[crypt_at..].starts_with("[minio-crypt-2]\ntype = crypt"),
            "the renamed section is the crypt one:\n{conf}"
        );
        assert!(
            conf.contains("[minio-crypt]\ntype = ftp"),
            "the real profile keeps its own name:\n{conf}"
        );
        assert_eq!(outcome.exported, 3, "base + overlay + second profile");
    }

    /// C-05, second half. A name the operator wrote by hand is not renamed
    /// behind their back: an `rclone sync vault:` they already have would
    /// silently start addressing something else.
    #[test]
    fn export_rclone_refuses_an_explicit_crypt_name_that_is_taken() {
        let servers = vec![
            export_server(
                "nas",
                "ftp",
                Some(serde_json::json!({
                    "rcloneCryptEnabled": true,
                    "rcloneCryptOverlayName": "vault",
                    "rcloneCryptPassword": "topsecret",
                })),
            ),
            export_server("vault", "ftp", None),
        ];
        let (outcome, conf) = export_to_string(&servers, "explicit-crypt");

        assert_eq!(conf.matches("[vault]\n").count(), 1, "one [vault]:\n{conf}");
        assert!(
            conf.contains("[vault]\ntype = ftp"),
            "[vault] is the real profile, not the overlay:\n{conf}"
        );
        assert!(
            !conf.contains("type = crypt"),
            "the overlay is refused, not renamed:\n{conf}"
        );
        assert!(
            !conf.contains("[vault-2]"),
            "an operator-chosen name is never suffixed:\n{conf}"
        );
        assert_eq!(outcome.exported, 2, "the two base remotes only");
        assert!(
            outcome.skipped.iter().any(|s| s.name == "nas"),
            "the refusal is reported, not silent: {:?}",
            outcome.skipped
        );
    }

    /// C-05, third half: the collision exists without crypt at all. The
    /// sanitizer maps every character outside rclone's set to `-` and collapses
    /// runs, so two different profile names can arrive at one remote name.
    #[test]
    fn export_rclone_separates_profiles_the_sanitizer_maps_together() {
        let servers = vec![
            export_server("nas(one)", "ftp", None),
            export_server("nas:one:", "ftp", None),
        ];
        let (outcome, conf) = export_to_string(&servers, "sanitizer-collision");

        assert_eq!(outcome.exported, 2, "both profiles are written:\n{conf}");
        assert_eq!(
            conf.matches("[nas-one]\n").count(),
            1,
            "the first keeps the sanitized name:\n{conf}"
        );
        assert!(
            conf.contains("[nas-one-2]"),
            "the second gets a free one:\n{conf}"
        );
    }

    /// C-05, fourth: a generated name must not take a real profile's name even
    /// when that profile is written later in the file.
    #[test]
    fn export_rclone_reserves_profile_names_before_generated_ones() {
        let servers = vec![
            export_server(
                "data",
                "s3",
                Some(serde_json::json!({ "bucket": "backup" })),
            ),
            export_server("data-backup", "ftp", None),
        ];
        let (_outcome, conf) = export_to_string(&servers, "reserved-first");

        assert_eq!(
            conf.matches("[data-backup]\n").count(),
            1,
            "one section with that name:\n{conf}"
        );
        assert!(
            conf.contains("[data-backup]\ntype = ftp"),
            "the real profile owns it even though the alias came first:\n{conf}"
        );
        assert!(
            conf.contains("[data-backup-2]\ntype = alias"),
            "the bucket alias steps aside:\n{conf}"
        );
    }

    /// C-13. rclone's zoho backend answers F6016 on every listing without the
    /// privatespace id. Discovery fills it in before the export, but returns
    /// silently on failed auth, on no network and on timeout, so a profile can
    /// reach the writer without one. Exporting it anyway reports a remote that
    /// lists nothing.
    #[test]
    fn export_rclone_skips_a_zoho_without_its_root_folder() {
        let servers = vec![export_server(
            "zoho",
            "zohoworkdrive",
            Some(serde_json::json!({ "region": "us" })),
        )];
        let (outcome, conf) = export_to_string(&servers, "zoho-no-root");

        assert_eq!(outcome.exported, 0, "nothing usable was written:\n{conf}");
        assert!(!conf.contains("type = zoho"), "no remote emitted:\n{conf}");
        assert_eq!(outcome.skipped.len(), 1, "{:?}", outcome.skipped);
        assert!(
            outcome.skipped[0].reason.contains("root_folder_id"),
            "the reason names what is missing: {:?}",
            outcome.skipped[0]
        );
    }

    #[test]
    fn export_rclone_keeps_a_zoho_that_has_its_root_folder() {
        let servers = vec![export_server(
            "zoho",
            "zohoworkdrive",
            Some(serde_json::json!({ "region": "us", "root_folder_id": "abc123" })),
        )];
        let (outcome, conf) = export_to_string(&servers, "zoho-with-root");

        assert_eq!(outcome.exported, 1, "{conf}");
        assert!(conf.contains("root_folder_id = abc123"), "{conf}");
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    }

    /// The cached login of an rclone `protondrive` remote, as rclone writes
    /// it: `client_salted_key_pass` is the base64 of the key password.
    const PROTON_REMOTE: &str = "[pd]\ntype = protondrive\nusername = someone@proton.me\n\
        password = NOT-A-REAL-OBSCURED-VALUE\nclient_uid = uid-1\n\
        client_access_token = access-1\nclient_refresh_token = refresh-1\n\
        client_salted_key_pass = a2V5LXBhc3N3b3JkLTE=\n";

    /// An rclone `protondrive` remote becomes a Proton Drive profile whose
    /// session (key password decoded) lands in the provider secrets, and the
    /// account password never comes along.
    #[test]
    fn a_protondrive_remote_imports_its_session_and_never_the_password() {
        let path = tmp_write(
            PROTON_REMOTE,
            &format!("aeroftp-test-proton-import-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let server = result
            .servers
            .iter()
            .find(|s| s.name == "pd")
            .expect("imported");
        assert_eq!(server.protocol.as_deref(), Some("proton"));
        assert_eq!(server.username, "someone@proton.me");
        assert!(
            server.credential.is_none(),
            "the account password stays out"
        );
        let mode = server
            .options
            .as_ref()
            .and_then(|o| o.get("proton_mode"))
            .cloned();
        assert_eq!(mode, Some(serde_json::json!(imported_proton_mode())));

        let secrets = result.provider_secrets.get(&server.id);
        if cfg!(feature = "proton-native") {
            let session: serde_json::Value = serde_json::from_str(
                secrets
                    .and_then(|s| s.proton_native_session.as_deref())
                    .expect("session imported"),
            )
            .unwrap();
            assert_eq!(
                session,
                serde_json::json!({
                    "uid": "uid-1",
                    "access_token": "access-1",
                    "refresh_token": "refresh-1",
                    "key_password": "key-password-1",
                })
            );
        } else {
            assert!(secrets
                .and_then(|s| s.proton_native_session.as_ref())
                .is_none());
        }
        assert_eq!(result.warnings.len(), 1, "the shared session is reported");
        // A Rust line continuation lost in an edit once left 18 spaces inside.
        assert!(
            !result.warnings[0].reason.contains("  "),
            "{:?}",
            result.warnings[0].reason
        );
    }

    #[test]
    fn a_protondrive_remote_without_a_complete_session_asks_for_a_sign_in() {
        let partial = PROTON_REMOTE.replace("client_refresh_token = refresh-1\n", "");
        let path = tmp_write(
            &partial,
            &format!("aeroftp-test-proton-partial-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let server = result
            .servers
            .iter()
            .find(|s| s.name == "pd")
            .expect("imported");
        assert!(!result.provider_secrets.contains_key(&server.id));
        assert!(
            result.warnings[0].reason.contains("sign in"),
            "{}",
            result.warnings[0].reason
        );
    }

    /// A native profile exports as rclone's cached login, and that file
    /// imports back to the very same session.
    #[test]
    fn a_native_proton_profile_round_trips_through_rclone() {
        let session = r#"{"uid":"uid-1","access_token":"access-1","refresh_token":"refresh-1","key_password":"key-password-1"}"#;
        let mut server = export_server(
            "pd",
            "proton",
            Some(serde_json::json!({
                "proton_mode": "native",
                PROTON_SESSION_OPTION: session,
            })),
        );
        server.username = "someone@proton.me".into();
        let (outcome, conf) = export_to_string(&[server], "proton-native");
        assert_eq!(outcome.exported, 1, "{conf}");
        for line in [
            "type = protondrive",
            "username = someone@proton.me",
            "client_uid = uid-1",
            "client_access_token = access-1",
            "client_refresh_token = refresh-1",
            "client_salted_key_pass = a2V5LXBhc3N3b3JkLTE=",
        ] {
            assert!(conf.contains(line), "missing {line:?} in\n{conf}");
        }
        assert!(
            !conf.contains("password ="),
            "no password is written:\n{conf}"
        );

        let path = tmp_write(
            &conf,
            &format!("aeroftp-test-proton-roundtrip-{}.conf", std::process::id()),
        );
        let back = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        if cfg!(feature = "proton-native") {
            let id = &back.servers[0].id;
            let imported = back.provider_secrets[id]
                .proton_native_session
                .clone()
                .expect("session back");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&imported).unwrap(),
                serde_json::from_str::<serde_json::Value>(session).unwrap()
            );
        }
    }

    #[test]
    fn a_cli_or_signed_out_proton_profile_is_skipped_with_its_reason() {
        let cli = export_server("cli", "proton", None);
        let signed_out = export_server(
            "out",
            "proton",
            Some(serde_json::json!({ "proton_mode": "native" })),
        );
        let (outcome, conf) = export_to_string(&[cli, signed_out], "proton-skipped");
        assert_eq!(outcome.exported, 0, "{conf}");
        assert!(!conf.contains("type = protondrive"), "{conf}");
        let reason = |name: &str| {
            outcome
                .skipped
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.reason.clone())
                .unwrap_or_default()
        };
        assert!(
            reason("cli").contains("Proton's own CLI"),
            "{}",
            reason("cli")
        );
        assert!(reason("out").contains("not signed in"), "{}", reason("out"));
    }

    /// The JSON the importer writes is the native client's `StoredSession`.
    #[cfg(feature = "proton-native")]
    #[test]
    fn the_imported_session_is_what_the_native_client_reads() {
        let mut remote = RcloneRemote::new();
        for (key, value) in [
            ("client_uid", "uid-1"),
            ("client_access_token", "access-1"),
            ("client_refresh_token", "refresh-1"),
            ("client_salted_key_pass", "a2V5LXBhc3N3b3JkLTE="),
        ] {
            remote.insert(key.to_string(), value.to_string());
        }
        let json = rclone_proton_session_to_aeroftp(&remote).expect("complete");
        let stored: crate::providers::proton_native::session::StoredSession =
            serde_json::from_str(&json).expect("a StoredSession");
        assert_eq!(stored.key_password, "key-password-1");
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh-1"));
    }

    /// L-10. An export where everything is refused used to be indistinguishable
    /// from a successful one: `Ok(0)` and a success envelope, while the
    /// adjacent "nothing selected" case exits 4.
    #[test]
    fn export_rclone_reports_what_it_left_out_when_it_wrote_nothing() {
        let servers = vec![export_server(
            "zoho-ca",
            "zohoworkdrive",
            Some(serde_json::json!({ "region": "ca", "root_folder_id": "abc123" })),
        )];
        let (outcome, _conf) = export_to_string(&servers, "zoho-region");

        assert_eq!(outcome.exported, 0);
        assert_eq!(outcome.skipped.len(), 1, "{:?}", outcome.skipped);
        assert_eq!(outcome.skipped[0].name, "zoho-ca");
        assert!(
            outcome.skipped[0].reason.contains("region"),
            "{:?}",
            outcome.skipped[0]
        );
    }

    /// L-11. The `[name]` header used to be written before the match that
    /// decides whether the protocol has an rclone backend, so the fall-through
    /// arm would have left a section with no `type =`, which rclone loads as a
    /// remote with no backend. Unreachable today only because both callers
    /// pre-filter, in a different file: this walks that gate and holds the
    /// writer to it, so the two cannot drift apart in silence.
    #[test]
    fn every_gated_protocol_writes_a_section_with_a_backend() {
        for proto in crate::bridge_shared::bridge_supported_protocols("rclone") {
            // Zoho needs both, Cloudinary its cloud name, ImageKit its
            // public key and Proton Drive a native session, or they are
            // skipped for the reasons above.
            let options = match *proto {
                "proton" => Some(serde_json::json!({
                    "proton_mode": "native",
                    PROTON_SESSION_OPTION: r#"{"uid":"u","access_token":"a","refresh_token":"r","key_password":"k"}"#,
                })),
                "zohoworkdrive" => {
                    Some(serde_json::json!({ "region": "us", "root_folder_id": "abc123" }))
                }
                "cloudinary" => Some(serde_json::json!({ "bucket": "demo-cloud" })),
                "imagekit" => Some(serde_json::json!({ "imagekit_public_key": "public_x" })),
                _ => None,
            };
            let servers = vec![export_server("remote", proto, options)];
            let (outcome, conf) = export_to_string(&servers, &format!("gate-{proto}"));

            assert_eq!(
                outcome.exported, 1,
                "protocol '{proto}' is exportable per the gate but wrote nothing:\n{conf}"
            );
            let section = conf
                .find("[remote]\n")
                .unwrap_or_else(|| panic!("protocol '{proto}' wrote no section:\n{conf}"));
            assert!(
                conf[section..].starts_with("[remote]\ntype = "),
                "protocol '{proto}' wrote a section with no backend:\n{conf}"
            );
        }
    }

    /// L-11, the other direction: a protocol the writer has no arm for must
    /// leave nothing behind at all, not a header waiting for a `type =`.
    #[test]
    fn a_protocol_without_an_rclone_backend_writes_no_section() {
        let servers = vec![
            export_server("peer-drive", "peer", None),
            export_server("peer-drive", "ftp", None),
        ];
        let (outcome, conf) = export_to_string(&servers, "no-backend");

        assert_eq!(outcome.exported, 1, "only the ftp profile:\n{conf}");
        assert_eq!(
            conf.matches("[peer-drive]\n").count(),
            1,
            "no orphan header, and the name is free for the ftp profile:\n{conf}"
        );
        assert!(
            conf.contains("[peer-drive]\ntype = ftp"),
            "the released name went to the profile that could use it:\n{conf}"
        );
        assert!(
            outcome.skipped.iter().any(|s| s.reason.contains("peer")),
            "{:?}",
            outcome.skipped
        );
    }

    /// CLAUDE-AV-B9-02: an uncapped parse let a single 10 MB config declare
    /// hundreds of thousands of remotes, each costing a whole-vault rewrite.
    #[test]
    fn parse_rclone_conf_stops_at_the_remote_cap() {
        let mut conf = String::new();
        for i in 0..(MAX_REMOTES + 500) {
            conf.push_str(&format!("[r{i}]\ntype = ftp\nhost = h\n"));
        }
        let sections = parse_rclone_conf(&conf);
        assert_eq!(sections.len(), MAX_REMOTES);
    }

    /// The cap must not disturb an ordinary config.
    #[test]
    fn parse_rclone_conf_keeps_every_remote_below_the_cap() {
        let conf = "[a]\ntype = ftp\nhost = h1\n\n[b]\ntype = sftp\nhost = h2\n";
        let sections = parse_rclone_conf(conf);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections["b"]["host"], "h2");
    }

    #[test]
    fn test_parse_ini() {
        let conf = r#"
[mynas]
type = sftp
host = 192.168.1.100
port = 22
user = admin
pass = some_obscured_value

[backup-s3]
type = s3
provider = AWS
access_key_id = AKIAIOSFODNN7EXAMPLE
secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
region = eu-west-1
"#;
        let sections = parse_rclone_conf(conf);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections["mynas"]["type"], "sftp");
        assert_eq!(sections["mynas"]["host"], "192.168.1.100");
        assert_eq!(sections["backup-s3"]["provider"], "AWS");
    }

    #[test]
    fn test_parse_webdav_url() {
        let (host, path, port) =
            parse_webdav_url("https://cloud.example.com/remote.php/dav/files/user/");
        assert_eq!(host, "cloud.example.com");
        assert_eq!(path, "/remote.php/dav/files/user/");
        assert_eq!(port, 443);

        let (host, path, port) = parse_webdav_url("http://localhost:8080/webdav");
        assert_eq!(host, "localhost");
        assert_eq!(path, "/webdav");
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_map_ftp() {
        let mut remote = HashMap::new();
        remote.insert("type".into(), "ftp".into());
        remote.insert("host".into(), "ftp.example.com".into());
        remote.insert("user".into(), "admin".into());
        remote.insert("port".into(), "21".into());

        let mapped = map_remote("test-ftp", &remote, None).expect("should map FTP");
        assert_eq!(mapped.protocol, "ftp");
        assert_eq!(mapped.host, "ftp.example.com");
        assert_eq!(mapped.port, 21);
        assert_eq!(mapped.username, "admin");
    }

    #[test]
    fn test_map_ftps() {
        let mut remote = HashMap::new();
        remote.insert("type".into(), "ftp".into());
        remote.insert("host".into(), "ftps.example.com".into());
        remote.insert("user".into(), "secure".into());
        remote.insert("tls".into(), "true".into());

        let mapped = map_remote("test-ftps", &remote, None).expect("should map FTPS");
        assert_eq!(mapped.protocol, "ftps");
    }

    #[test]
    fn test_map_unsupported() {
        let mut remote = HashMap::new();
        remote.insert("type".into(), "fichier".into());

        assert!(map_remote("unsupported", &remote, None).is_err());
    }

    /// Issue #214: rclone stores `token = {"access_token", "refresh_token",
    /// "expiry"}` per `[remote]`. The importer converts the blob to the
    /// AeroFTP `StoredTokens` shape (Unix `expires_at` instead of RFC 3339
    /// `expiry`) so the destination vault writes it back verbatim.
    #[test]
    fn test_rclone_token_to_aeroftp_full_conversion() {
        let blob = r#"{
            "access_token": "ya29.a0AfH6SM",
            "token_type": "Bearer",
            "refresh_token": "1//04abcdef",
            "expiry": "2030-01-01T12:00:00Z"
        }"#;
        let aero = rclone_token_to_aeroftp(blob).expect("blob should convert");
        let parsed: serde_json::Value = serde_json::from_str(&aero).unwrap();
        assert_eq!(parsed["access_token"], "ya29.a0AfH6SM");
        assert_eq!(parsed["refresh_token"], "1//04abcdef");
        assert_eq!(parsed["token_type"], "Bearer");
        // 2030-01-01T12:00:00Z == 1893499200
        assert_eq!(parsed["expires_at"].as_i64().unwrap(), 1893499200);
        assert!(parsed["scopes"].is_array());
    }

    /// A blob without `access_token` is unusable: the importer must skip it
    /// rather than write a half-formed record into the destination vault.
    #[test]
    fn test_rclone_token_to_aeroftp_rejects_missing_access_token() {
        let blob = r#"{"refresh_token": "r", "expiry": "2030-01-01T00:00:00Z"}"#;
        assert!(rclone_token_to_aeroftp(blob).is_none());
    }

    /// A blob with an empty refresh_token serialises with `refresh_token:
    /// null` so the destination vault treats the entry as a refreshless
    /// session rather than trying to use the empty string as a refresh
    /// token.
    #[test]
    fn test_rclone_token_to_aeroftp_normalises_empty_refresh() {
        let blob =
            r#"{"access_token": "a", "refresh_token": "", "expiry": "2030-01-01T00:00:00Z"}"#;
        let aero = rclone_token_to_aeroftp(blob).expect("blob should convert");
        let parsed: serde_json::Value = serde_json::from_str(&aero).unwrap();
        assert!(parsed["refresh_token"].is_null());
    }

    /// Issue #214: an rclone Google Drive remote that carries `token = {...}`
    /// produces a `MappedProfile` with the converted OAuth blob ready for
    /// vault-side storage under `oauth_google_<profile_id>`.
    #[test]
    fn test_map_drive_extracts_oauth_token() {
        let mut remote = HashMap::new();
        remote.insert("type".into(), "drive".into());
        remote.insert(
            "token".into(),
            r#"{"access_token":"abc","token_type":"Bearer","refresh_token":"r","expiry":"2030-01-01T00:00:00Z"}"#
                .into(),
        );
        let mapped = map_remote("my-drive", &remote, None).expect("should map drive");
        assert_eq!(mapped.protocol, "googledrive");
        let oauth_json = mapped.oauth_token.expect("token must be propagated");
        let parsed: serde_json::Value = serde_json::from_str(&oauth_json).unwrap();
        assert_eq!(parsed["access_token"], "abc");
        assert_eq!(parsed["refresh_token"], "r");
    }

    /// Issue #214: rclone Jotta remotes encode the refresh token inside the
    /// same `token = {...}` shape. The importer reshapes it to the local
    /// persistence layout (refresh_token / token_endpoint / username) so the
    /// destination provider reconnects without burning the single-use login
    /// token.
    #[test]
    fn test_map_jottacloud_extracts_refresh_token() {
        let mut remote = HashMap::new();
        remote.insert("type".into(), "jottacloud".into());
        remote.insert("user".into(), "alice@example.com".into());
        remote.insert(
            "token".into(),
            r#"{"access_token":"a","refresh_token":"rotated"}"#.into(),
        );
        let mapped = map_remote("jotta", &remote, None).expect("should map jotta");
        assert_eq!(mapped.protocol, "jottacloud");
        assert_eq!(mapped.username, "alice@example.com");
        let refresh_json = mapped.jotta_refresh.expect("refresh must be propagated");
        let parsed: serde_json::Value = serde_json::from_str(&refresh_json).unwrap();
        assert_eq!(parsed["refresh_token"], "rotated");
        assert_eq!(parsed["username"], "alice@example.com");
    }

    #[test]
    fn test_map_crypt_overlay_on_base_remote() {
        let mut sections: HashMap<String, RcloneRemote> = HashMap::new();

        let mut base = HashMap::new();
        base.insert("type".into(), "sftp".into());
        base.insert("host".into(), "192.168.1.10".into());
        base.insert("port".into(), "22".into());
        base.insert("user".into(), "admin".into());
        sections.insert("mynas".into(), base);

        let mut crypt = HashMap::new();
        crypt.insert("type".into(), "crypt".into());
        crypt.insert("remote".into(), "mynas:/encrypted".into());
        // `rclone obscure topsecret` and `rclone obscure saltsecret` (rclone
        // v1.75.1): rclone refuses a crypt remote whose passwords are not
        // obscured ("is it obscured?"), so plain text here is not a config
        // rclone could have written.
        crypt.insert(
            "password".into(),
            "I0foQLrVcrxA3fTR32MLs51K2uCFdW1sgw".into(),
        );
        crypt.insert(
            "password2".into(),
            "R7Q09CWMcogQNXenlf7eB09NfisKl3wFr9k".into(),
        );
        crypt.insert("filename_encryption".into(), "standard".into());
        crypt.insert("directory_name_encryption".into(), "true".into());

        let mapped = map_crypt_remote("mycrypt", &crypt, &sections, None)
            .expect("should map crypt")
            .0;
        assert_eq!(mapped.protocol, "sftp");
        assert_eq!(mapped.host, "192.168.1.10");
        assert_eq!(mapped.initial_path.as_deref(), Some("/encrypted"));

        let opts = mapped.options.expect("options should exist");
        let obj = opts.as_object().expect("options must be object");
        assert_eq!(
            obj.get("rcloneCryptEnabled").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            obj.get("rcloneCryptRemote").and_then(|v| v.as_str()),
            Some("mynas:/encrypted")
        );
        assert_eq!(
            obj.get("rcloneCryptPassword").and_then(|v| v.as_str()),
            Some("topsecret")
        );
        assert_eq!(
            obj.get("rcloneCryptPassword2").and_then(|v| v.as_str()),
            Some("saltsecret")
        );
        assert_eq!(
            obj.get("rcloneCryptFilenameEncryption")
                .and_then(|v| v.as_str()),
            Some("standard")
        );
        assert_eq!(
            obj.get("rcloneCryptDirectoryNameEncryption")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_reveal_obscured_password() {
        // Generated with: rclone obscure "testpassword123"
        let obscured = "LZ9RxVK9L7SryViTF1LcFaIhT4Pe_wQkOD3Gud9FnQ";
        let revealed = reveal_obscured(obscured).expect("should reveal password");
        assert_eq!(revealed, "testpassword123");
    }

    #[test]
    fn test_reveal_empty_returns_error() {
        assert!(reveal_obscured("").is_err() || reveal_obscured("short").is_err());
    }

    #[test]
    fn test_full_import() {
        use std::io::Write;
        let conf = r#"
[my-nas]
type = sftp
host = 192.168.1.100
port = 22
user = admin
pass = LZ9RxVK9L7SryViTF1LcFaIhT4Pe_wQkOD3Gud9FnQ

[gdrive]
type = drive
token = {"access_token":"fake"}

[unsupported-thing]
type = fichier
"#;
        let tmp = std::env::temp_dir().join("aeroftp-test-rclone.conf");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(conf.as_bytes()).unwrap();
        }
        let result = import_rclone(&tmp).expect("should parse");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(result.total_remotes, 3);
        assert_eq!(result.servers.len(), 2); // sftp + drive
        assert_eq!(result.skipped.len(), 1); // fichier

        // Verify SFTP mapping
        let sftp = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("sftp"))
            .unwrap();
        assert_eq!(sftp.host, "192.168.1.100");
        assert_eq!(sftp.port, 22);
        assert_eq!(sftp.username, "admin");
        assert_eq!(sftp.credential.as_deref(), Some("testpassword123"));

        // Verify Google Drive mapping (no credential, OAuth)
        let gdrive = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("googledrive"))
            .unwrap();
        assert!(gdrive.credential.is_none());

        // Verify skipped
        assert_eq!(result.skipped[0].rclone_type, "fichier");
    }

    #[test]
    fn test_import_s3_other_provider_refines_via_endpoint() {
        use std::io::Write;
        let conf = r#"
[filebase]
type = s3
provider = Other
access_key_id = AK
secret_access_key = SK
endpoint = https://s3.filebase.io
region = auto
bucket = aero-base

[plain-custom]
type = s3
provider = Other
access_key_id = AK
secret_access_key = SK
endpoint = https://s3.example-unknown.com

[tigris]
type = s3
provider = Other
access_key_id = AK
secret_access_key = SK
endpoint = https://t3.storage.dev
region = auto
force_path_style = false
bucket = aero-tigris

[tigris-no-region]
type = s3
provider = Other
access_key_id = AK
secret_access_key = SK
endpoint = https://t3.storage.dev

[tigris-blank-region]
type = s3
provider = Other
access_key_id = AK
secret_access_key = SK
endpoint = https://fly.storage.tigris.dev
region =

[filebase-no-region]
type = s3
provider = Other
access_key_id = AK
secret_access_key = SK
endpoint = https://s3.filebase.io
"#;
        let tmp = std::env::temp_dir().join("aeroftp-test-rclone-s3-refine.conf");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(conf.as_bytes()).unwrap();
        }
        let result = import_rclone(&tmp).expect("should parse");
        std::fs::remove_file(&tmp).ok();

        let fb = result
            .servers
            .iter()
            .find(|s| s.name == "filebase")
            .expect("filebase remote");
        assert_eq!(fb.provider_id.as_deref(), Some("filebase"));
        let opts = fb.options.as_ref().expect("options");
        assert_eq!(
            opts.get("bucket").and_then(|v| v.as_str()),
            Some("aero-base")
        );

        let tg = result
            .servers
            .iter()
            .find(|s| s.name == "tigris")
            .expect("tigris remote");
        assert_eq!(tg.provider_id.as_deref(), Some("tigris"));
        assert_eq!(
            tg.options
                .as_ref()
                .and_then(|o| o.get("pathStyle"))
                .and_then(|v| v.as_bool()),
            Some(false)
        );

        // No region, or a blank one, is `auto` on Tigris and stays `us-east-1`
        // on every other S3.
        let region_of = |name: &str| {
            result
                .servers
                .iter()
                .find(|s| s.name == name)
                .and_then(|s| s.options.as_ref())
                .and_then(|o| o.get("region"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        assert_eq!(region_of("tigris-no-region").as_deref(), Some("auto"));
        assert_eq!(region_of("tigris-blank-region").as_deref(), Some("auto"));
        assert_eq!(region_of("tigris").as_deref(), Some("auto"));
        assert_eq!(
            region_of("filebase-no-region").as_deref(),
            Some("us-east-1")
        );

        // Unknown endpoints must stay generic.
        let plain = result
            .servers
            .iter()
            .find(|s| s.name == "plain-custom")
            .expect("plain-custom remote");
        assert_eq!(plain.provider_id.as_deref(), Some("custom-s3"));
    }

    #[test]
    fn test_import_rclone_orders_remotes_alphabetically() {
        use std::io::Write;
        // Remotes deliberately listed out of order. parse_rclone_conf stores
        // them in a HashMap, so without an explicit sort the import surfaced
        // them in a randomized order. import_rclone must emit them sorted by
        // name, mirroring `rclone listremotes`.
        let conf = r#"
[zulu]
type = ftp
host = zulu.example.com
user = z

[mike]
type = ftp
host = mike.example.com
user = m

[alpha]
type = ftp
host = alpha.example.com
user = a

[tango]
type = ftp
host = tango.example.com
user = t
"#;
        let tmp = std::env::temp_dir().join("aeroftp-test-rclone-order.conf");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(conf.as_bytes()).unwrap();
        }
        let result = import_rclone(&tmp).expect("should parse");
        std::fs::remove_file(&tmp).ok();

        let names: Vec<&str> = result.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mike", "tango", "zulu"]);
    }

    #[test]
    fn test_obscure_reveal_roundtrip() {
        let passwords = [
            "hello",
            "p@ssw0rd!",
            "with spaces",
            "unicode: \u{00e9}\u{00f1}",
            "",
        ];
        for pw in &passwords {
            if pw.is_empty() {
                continue; // empty password has no meaningful obscure
            }
            let obscured = obscure_password(pw).expect("should obscure");
            let revealed = reveal_obscured(&obscured).expect("should reveal");
            assert_eq!(&revealed, pw, "roundtrip failed for: {}", pw);
        }
    }

    #[test]
    fn test_export_rclone() {
        let servers = vec![
            RcloneExportServer {
                id: "test-sftp".to_string(),
                name: "test-sftp".to_string(),
                host: "192.168.1.1".to_string(),
                port: 22,
                username: "admin".to_string(),
                protocol: Some("sftp".to_string()),
                options: None,
                provider_id: None,
            },
            RcloneExportServer {
                id: "my-s3".to_string(),
                name: "my-s3".to_string(),
                host: "s3.amazonaws.com".to_string(),
                port: 443,
                username: "AKIAEXAMPLE".to_string(),
                protocol: Some("s3".to_string()),
                options: Some(serde_json::json!({"region": "eu-west-1", "bucket": "mybucket"})),
                provider_id: Some("amazon-s3".to_string()),
            },
        ];
        let mut passwords = HashMap::new();
        passwords.insert("test-sftp".to_string(), "secret123".to_string());
        passwords.insert("my-s3".to_string(), "s3secret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-rclone.conf");
        let exported = export_rclone(&servers, &passwords, &tmp)
            .expect("should export")
            .exported;
        // Two profiles; the pinned bucket adds its alias remote.
        assert_eq!(exported, 3);

        // Verify the exported file can be re-imported
        let result = import_rclone(&tmp).expect("should reimport");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(result.servers.len(), 2);

        let sftp = result
            .servers
            .iter()
            .find(|s| s.name == "test-sftp")
            .unwrap();
        assert_eq!(sftp.protocol.as_deref(), Some("sftp"));
        assert_eq!(sftp.credential.as_deref(), Some("secret123"));

        let s3 = result.servers.iter().find(|s| s.name == "my-s3").unwrap();
        assert_eq!(s3.protocol.as_deref(), Some("s3"));
        assert_eq!(s3.credential.as_deref(), Some("s3secret"));
    }

    #[test]
    fn test_export_rclone_jottacloud_emits_refreshable_token() {
        // Jotta authenticates via an OIDC refresh blob, passed in through the
        // password slot in the shape AeroFTP persists. The exported remote must
        // carry a full oauth2 token (non-empty access_token placeholder +
        // refresh_token + past expiry) and `configVersion = 1`, otherwise
        // rclone rejects it ("outdated config" / "no refresh token").
        let servers = vec![RcloneExportServer {
            id: "Jotta".to_string(),
            name: "Jotta".to_string(),
            host: "jfs.jottacloud.com".to_string(),
            port: 443,
            username: "deviceuser".to_string(),
            protocol: Some("jottacloud".to_string()),
            options: None,
            provider_id: Some("jottacloud".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert(
            "Jotta".to_string(),
            r#"{"refresh_token":"rt-secret","token_endpoint":"https://id.jottacloud.com/auth/realms/jottacloud/protocol/openid-connect/token","username":"deviceuser"}"#
                .to_string(),
        );

        let tmp = std::env::temp_dir().join("aeroftp-test-export-rclone-jotta.conf");
        let exported = export_rclone(&servers, &passwords, &tmp)
            .expect("should export")
            .exported;
        assert_eq!(exported, 1);
        let content = std::fs::read_to_string(&tmp).expect("read export");
        std::fs::remove_file(&tmp).ok();

        assert!(content.contains("type = jottacloud"));
        assert!(content.contains("user = deviceuser"));
        assert!(content.contains("\"refresh_token\":\"rt-secret\""));
        assert!(content.contains("\"access_token\":\"expired\""));
        assert!(content.contains(
            "token_endpoint = https://id.jottacloud.com/auth/realms/jottacloud/protocol/openid-connect/token"
        ));
        assert!(content.contains("configVersion = 1"));
    }

    #[test]
    fn test_export_rclone_ftps_uses_explicit_tls_only() {
        // rclone treats `tls = true` as implicit FTPS and rejects it when
        // `explicit_tls = true` is also present. An explicit-TLS profile
        // (`tlsMode`, the key the GUI stores) must set only `explicit_tls`.
        let servers = vec![RcloneExportServer {
            id: "secure-ftp".to_string(),
            name: "secure-ftp".to_string(),
            host: "ftp.example.com".to_string(),
            port: 21,
            username: "alice".to_string(),
            protocol: Some("ftps".to_string()),
            options: Some(serde_json::json!({ "tlsMode": "explicit" })),
            provider_id: None,
        }];
        let mut passwords = HashMap::new();
        passwords.insert("secure-ftp".to_string(), "secret123".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-ftps.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("explicit_tls = true"),
            "explicit FTPS flag must be present:\n{conf}"
        );
        assert!(
            !conf.contains("\ntls = true\n"),
            "must not also enable implicit FTPS:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_ftp_tls_follows_the_connection_mode() {
        // The TLS the rclone remote uses is the TLS AeroFTP connects with: an
        // `ftp` profile with explicit TLS used to become a cleartext remote,
        // and an implicit `ftps` profile an explicit one.
        let mk = |name: &str, protocol: &str, mode: Option<&str>| RcloneExportServer {
            id: name.to_string(),
            name: name.to_string(),
            host: "ftp.example.com".to_string(),
            port: 21,
            username: "alice".to_string(),
            protocol: Some(protocol.to_string()),
            options: mode.map(|m| serde_json::json!({ "tlsMode": m })),
            provider_id: None,
        };
        let servers = vec![
            mk("ftp-explicit", "ftp", Some("explicit")),
            mk("ftps-default", "ftps", None),
            mk("ftp-plain", "ftp", None),
        ];
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-test-export-ftp-tls-{}.conf",
            crate::bridge_shared::uuid_v4()
        ));
        export_rclone(&servers, &HashMap::new(), &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();
        let section = |name: &str| {
            let start = conf.find(&format!("[{name}]")).expect(name);
            let rest = &conf[start + 1..];
            let end = rest
                .find("\n[")
                .map(|e| start + 1 + e)
                .unwrap_or(conf.len());
            conf[start..end].to_string()
        };
        assert!(
            section("ftp-explicit").contains("explicit_tls = true"),
            "{conf}"
        );
        // Same default as the connection: `ftps` without a mode is implicit.
        assert!(section("ftps-default").contains("\ntls = true"), "{conf}");
        assert!(!section("ftps-default").contains("explicit_tls"), "{conf}");
        assert!(!section("ftp-plain").contains("tls"), "{conf}");

        // Both TLS modes carry `disable_tls13`, and a cleartext remote carries
        // nothing. Measured against the lab vsftpd on 2026-09-19: without this
        // line rclone fails the transfer with `426 Failure reading network
        // stream`, because RFC 4217 section 10.2 wants the data connection to
        // resume the control connection's session and a TLS 1.3 ticket is
        // single-use. `providers::ftp::make_tls_connector` pins TLS 1.2 for the
        // same reason, so an export without this hands the user a remote that
        // this client already knows cannot work.
        assert!(
            section("ftp-explicit").contains("disable_tls13 = true"),
            "{conf}"
        );
        assert!(
            section("ftps-default").contains("disable_tls13 = true"),
            "{conf}"
        );
        assert!(!section("ftp-plain").contains("disable_tls13"), "{conf}");
    }

    #[test]
    fn test_export_rclone_sftp_pins_known_hosts_and_carries_start_folder() {
        // The exported SFTP remote must keep the profile's trust posture (host
        // key checked against ~/.ssh/known_hosts) and its starting folder,
        // which rclone can only express as an alias remote.
        let servers = vec![RcloneExportServer {
            id: "nas".to_string(),
            name: "nas".to_string(),
            host: "nas.example.test".to_string(),
            port: 2222,
            username: "nasuser".to_string(),
            protocol: Some("sftp".to_string()),
            options: Some(serde_json::json!({
                "initial_path": "/mnt/data/sync"
            })),
            provider_id: None,
        }];
        let mut passwords = HashMap::new();
        passwords.insert("nas".to_string(), "secret".to_string());
        let tmp = std::env::temp_dir().join("aeroftp-test-export-sftp-path-alias.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("known_hosts_file = ~/.ssh/known_hosts"),
            "sftp remote must validate host keys like the profile does:\n{conf}"
        );
        assert!(
            conf.contains("[nas-path]"),
            "expected start-folder alias:\n{conf}"
        );
        assert!(
            conf.contains("remote = nas:/mnt/data/sync"),
            "sftp alias keeps the absolute folder:\n{conf}"
        );
        // The alias is a section of its own, after the sftp section.
        let sftp_at = conf.find("[nas]").expect("sftp section");
        let alias_at = conf.find("[nas-path]").expect("alias section");
        assert!(alias_at > sftp_at);
    }

    /// rclone reads `remote = nas:/mnt/data ` as `nas:/mnt/data` (measured with
    /// rclone 1.75.2; a quoted value keeps its quotes): the alias would open
    /// another folder, so it is left out and the config says why.
    #[test]
    fn test_export_rclone_leaves_out_a_start_folder_that_ends_in_a_space() {
        let servers = vec![RcloneExportServer {
            id: "nas".to_string(),
            name: "nas".to_string(),
            host: "nas.example.test".to_string(),
            port: 22,
            username: "nasuser".to_string(),
            protocol: Some("sftp".to_string()),
            options: Some(serde_json::json!({ "initial_path": "/mnt/data " })),
            provider_id: None,
        }];
        let mut passwords = HashMap::new();
        passwords.insert("nas".to_string(), "secret".to_string());
        let tmp = std::env::temp_dir().join("aeroftp-test-export-sftp-path-trailing-space.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("[nas]"),
            "the remote itself is written:\n{conf}"
        );
        assert!(
            !conf.contains("[nas-path]"),
            "no alias to another folder:\n{conf}"
        );
        assert!(!conf.contains("remote = nas:/mnt/data"), "{conf}");
        assert!(
            conf.contains("# start-folder alias for 'nas' omitted: rclone's config cannot hold '/mnt/data ', whose name ends in a space"),
            "the config says why:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_scoped_crypt_wraps_the_start_folder_alias() {
        let servers = vec![RcloneExportServer {
            id: "nas".to_string(),
            name: "nas".to_string(),
            host: "nas.example.test".to_string(),
            port: 22,
            username: "u".to_string(),
            protocol: Some("sftp".to_string()),
            options: Some(serde_json::json!({
                "initial_path": "/team",
                "rcloneCryptEnabled": true,
                "rcloneCryptOverlayScope": "/vault",
                "rcloneCryptPassword": "pw"
            })),
            provider_id: None,
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-export-crypt-path-alias.conf");
        export_rclone(&servers, &HashMap::new(), &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();
        assert!(
            conf.contains("remote = nas-path:/vault") || conf.contains("remote = nas-path:vault"),
            "the crypt remote must wrap the start-folder alias, not the account root:\n{conf}"
        );
        assert!(
            !conf.contains("remote = nas:/vault"),
            "must not bypass the alias:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_root_folder_carries_no_alias_and_webdav_alias_is_relative() {
        let root = vec![RcloneExportServer {
            id: "plainftp".to_string(),
            name: "plainftp".to_string(),
            host: "ftp.example.test".to_string(),
            port: 21,
            username: "u".to_string(),
            protocol: Some("ftp".to_string()),
            options: Some(serde_json::json!({ "initial_path": "/" })),
            provider_id: None,
        }];
        let dav = vec![RcloneExportServer {
            id: "dav".to_string(),
            name: "dav".to_string(),
            host: "https://dav.example.test/".to_string(),
            port: 443,
            username: "u".to_string(),
            protocol: Some("webdav".to_string()),
            options: Some(serde_json::json!({ "initial_path": "/team/docs" })),
            provider_id: None,
        }];
        for (servers, tag) in [(root, "root"), (dav, "dav")] {
            let tmp = std::env::temp_dir().join(format!("aeroftp-test-export-path-{tag}.conf"));
            export_rclone(&servers, &HashMap::new(), &tmp).expect("should export");
            let conf = std::fs::read_to_string(&tmp).expect("read conf");
            std::fs::remove_file(&tmp).ok();
            if tag == "root" {
                assert!(
                    !conf.contains("type = alias"),
                    "a root start folder is the remote itself:\n{conf}"
                );
            } else {
                assert!(
                    conf.contains("remote = dav:team/docs"),
                    "webdav alias path is relative to the URL root:\n{conf}"
                );
            }
        }
    }

    #[test]
    fn test_export_rclone_s3_bucket_alias() {
        // F1 regression: a profile-pinned bucket must NOT be emitted as the
        // inert `bucket =` s3 key (silently ignored by rclone), but as an
        // `alias` remote that is bucket-relative, matching how the AeroFTP
        // S3 client writes keys.
        let servers = vec![RcloneExportServer {
            id: "minio".to_string(),
            name: "minio".to_string(),
            host: "s3.lab.example.test".to_string(),
            port: 443,
            username: "AKIAEXAMPLE".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({
                "region": "us-east-1",
                "endpoint": "https://s3.lab.example.test",
                "bucket": "aeroftp-test"
            })),
            provider_id: Some("minio".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("minio".to_string(), "s3secret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-s3-alias.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        // No inert bucket key in the s3 backend section.
        assert!(
            conf.contains("type = s3\ndirectory_markers = true\n"),
            "the S3 backend must recognize AeroFTP empty-directory markers"
        );
        assert!(
            !conf.contains("\nbucket = "),
            "must not emit the ignored s3 `bucket =` key:\n{conf}"
        );
        // Bucket-relative alias remote present and correct.
        assert!(
            conf.contains("[minio-aeroftp-test]"),
            "expected bucket alias section:\n{conf}"
        );
        assert!(
            conf.contains("type = alias"),
            "alias remote must be type=alias:\n{conf}"
        );
        assert!(
            conf.contains("remote = minio:aeroftp-test"),
            "alias must point at <remote>:<bucket>:\n{conf}"
        );

        // The alias section is not re-imported as a phantom server.
        let result = import_rclone(&tmp).unwrap_or_else(|_| {
            // file already removed; re-export to a fresh path for re-import
            let p = std::env::temp_dir().join("aeroftp-test-export-s3-alias2.conf");
            export_rclone(&servers, &passwords, &p).unwrap();
            let r = import_rclone(&p).unwrap();
            std::fs::remove_file(&p).ok();
            r
        });
        assert_eq!(result.servers.len(), 1, "alias must not become a server");
    }

    #[test]
    fn test_export_rclone_swift_key_is_plaintext() {
        // Bug (2026-06-07): swift `key` was emitted obscured, but rclone's
        // swift backend does NOT mark `key` as `IsPassword: true`, so it uses
        // the value verbatim. An obscured key was sent as the literal password
        // and authentication failed (HTTP 401). It must be emitted plain, like
        // S3 `secret_access_key` and Azure `key`. Verified against
        // `rclone config providers` (swift.key IsPassword = false).
        let servers = vec![RcloneExportServer {
            id: "blomp".to_string(),
            name: "blomp".to_string(),
            host: "authenticate.blomp.com".to_string(),
            port: 443,
            username: "user@example.com".to_string(),
            protocol: Some("swift".to_string()),
            options: Some(serde_json::json!({
                "endpoint": "https://authenticate.blomp.com",
                "tenant": "storage"
            })),
            provider_id: Some("blomp".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("blomp".to_string(), "148%BlomPass".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-swift.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("key = 148%BlomPass"),
            "swift key must be emitted plain, not obscured:\n{conf}"
        );
        // Round-trips back to the same plaintext on import.
        let p = std::env::temp_dir().join("aeroftp-test-export-swift2.conf");
        export_rclone(&servers, &passwords, &p).unwrap();
        let result = import_rclone(&p).unwrap();
        std::fs::remove_file(&p).ok();
        let swift = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("swift"))
            .expect("swift server present");
        assert_eq!(
            swift.credential.as_deref(),
            Some("148%BlomPass"),
            "swift key must round-trip as plaintext"
        );
    }

    #[test]
    fn test_export_rclone_filen_roundtrip() {
        // #128: a Filen profile exports to rclone's `filen` backend as
        // `email` + obscured `password` + obscured `api_key`, and imports back
        // to the same account (password as the credential, api_key relocated
        // into options.filen_api_key for the vault).
        let servers = vec![RcloneExportServer {
            id: "filen-acct".to_string(),
            name: "filen-acct".to_string(),
            host: "filen.io".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("filen".to_string()),
            options: Some(serde_json::json!({ "filen_api_key": "secret-cli-key" })),
            provider_id: Some("filen".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("filen-acct".to_string(), "S3cr3tPass!".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-filen.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(conf.contains("type = filen"), "missing type:\n{conf}");
        assert!(
            conf.contains("email = me@example.com"),
            "missing email:\n{conf}"
        );
        assert!(
            conf.contains("password = ") && !conf.contains("password = S3cr3tPass!"),
            "password must be present and obscured:\n{conf}"
        );
        assert!(
            conf.contains("api_key = ") && !conf.contains("api_key = secret-cli-key"),
            "api_key must be present and obscured:\n{conf}"
        );

        let result = import_rclone(&tmp_write(&conf, "aeroftp-test-import-filen.conf")).unwrap();
        let filen = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("filen"))
            .expect("filen server present");
        assert_eq!(filen.username, "me@example.com");
        assert_eq!(
            filen.credential.as_deref(),
            Some("S3cr3tPass!"),
            "password must round-trip"
        );
        assert_eq!(
            filen
                .options
                .as_ref()
                .and_then(|o| o.get("filen_api_key"))
                .and_then(|v| v.as_str()),
            Some("secret-cli-key"),
            "api_key must round-trip into options.filen_api_key"
        );
    }

    /// Export `servers` with `passwords` keyed by profile name, return the
    /// outcome and the file text.
    fn export_with_passwords(
        servers: &[RcloneExportServer],
        passwords: &[(&str, &str)],
        tag: &str,
    ) -> (RcloneExportOutcome, String) {
        let passwords: HashMap<String, String> = passwords
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-test-export-{}-{}.conf",
            tag,
            std::process::id()
        ));
        let outcome = export_rclone(servers, &passwords, &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read export");
        std::fs::remove_file(&tmp).ok();
        (outcome, conf)
    }

    /// The comment block directly above `[name]`, and the lines of that
    /// section up to the next blank line.
    fn notes_and_body<'a>(conf: &'a str, name: &str) -> (Vec<&'a str>, Vec<&'a str>) {
        let lines: Vec<&str> = conf.lines().collect();
        let header = format!("[{}]", name);
        let at = lines
            .iter()
            .position(|l| *l == header)
            .unwrap_or_else(|| panic!("no section {header}:\n{conf}"));
        let mut notes: Vec<&str> = lines[..at]
            .iter()
            .rev()
            .take_while(|l| l.starts_with('#'))
            .copied()
            .collect();
        notes.reverse();
        let body = lines[at + 1..]
            .iter()
            .take_while(|l| !l.trim().is_empty())
            .copied()
            .collect();
        (notes, body)
    }

    fn internxt_server(name: &str) -> RcloneExportServer {
        RcloneExportServer {
            id: name.to_string(),
            name: name.to_string(),
            host: "gateway.internxt.com".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("internxt".to_string()),
            options: None,
            provider_id: Some("internxt".to_string()),
        }
    }

    #[test]
    fn test_export_rclone_internxt_roundtrip() {
        // Internxt used to be refused as "not exportable", with help text that
        // called it OAuth. rclone's `internxt` backend takes the account email
        // and password, which is what AeroFTP holds.
        assert!(
            crate::bridge_shared::bridge_supported_protocols("rclone").contains(&"internxt"),
            "the CLI and the GUI export only what this list names"
        );
        let (outcome, conf) = export_with_passwords(
            &[internxt_server("internxt-acct")],
            &[("internxt-acct", "S3cr3tPass!")],
            "internxt",
        );

        assert_eq!(outcome.exported, 1, "{conf}");
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
        let (notes, body) = notes_and_body(&conf, "internxt-acct");
        assert_eq!(body[0], "type = internxt", "{conf}");
        assert!(body.contains(&"email = me@example.com"), "{conf}");
        let pass_line = body
            .iter()
            .find_map(|l| l.strip_prefix("pass = "))
            .unwrap_or_else(|| panic!("missing pass:\n{conf}"));
        assert_ne!(pass_line, "S3cr3tPass!", "pass must be obscured");
        assert_eq!(reveal_obscured(pass_line).as_deref(), Ok("S3cr3tPass!"));
        // rclone refuses the remote until its own sign-in stores the mnemonic,
        // so the file names the command that does it, for this remote, and
        // says which plans rclone can sign in on at all.
        let notes = notes.join("\n");
        assert!(
            notes.contains("run `rclone config reconnect \"internxt-acct:\"` once before use"),
            "{conf}"
        );
        assert!(notes.contains("402"), "plan restriction missing:\n{conf}");

        let path = tmp_write(
            &conf,
            &format!("aeroftp-test-import-internxt-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let internxt = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("internxt"))
            .expect("internxt server present");
        assert_eq!(internxt.username, "me@example.com");
        assert_eq!(internxt.host, "gateway.internxt.com");
        assert_eq!(internxt.provider_id.as_deref(), Some("internxt"));
        assert_eq!(
            internxt.credential.as_deref(),
            Some("S3cr3tPass!"),
            "password must round-trip"
        );
        assert!(result.warnings.is_empty());
    }

    /// Exported without credentials (the GUI "Include credentials" switch
    /// off): `rclone config reconnect` would sign in with an empty password,
    /// since rclone's internxt Config does not ask for a missing one, so the
    /// file must point at the command that asks for it instead.
    #[test]
    fn test_export_rclone_internxt_without_password_says_how_to_set_it() {
        for passwords in [&[][..], &[("internxt-acct", "")][..]] {
            let (outcome, conf) = export_with_passwords(
                &[internxt_server("internxt-acct")],
                passwords,
                "internxt-nopass",
            );
            assert_eq!(outcome.exported, 1, "{conf}");
            let (notes, body) = notes_and_body(&conf, "internxt-acct");
            assert!(
                !body.iter().any(|l| l.starts_with("pass")),
                "no pass line without a password:\n{conf}"
            );
            let notes = notes.join("\n");
            assert!(notes.contains("no password was exported"), "{conf}");
            assert!(
                notes.contains("`rclone config update \"internxt-acct\" --all`"),
                "{conf}"
            );
            assert!(
                !notes.contains("config reconnect"),
                "a reconnect cannot work without a password:\n{conf}"
            );
        }
    }

    /// rclone accepts spaces in a remote name, so a command written into the
    /// file must keep the name one shell argument.
    #[test]
    fn test_export_rclone_quotes_a_remote_name_with_a_space() {
        let mut dropbox = export_server("My Dropbox", "dropbox", None);
        dropbox.provider_id = Some("dropbox".to_string());
        let (_, conf) = export_with_passwords(
            &[internxt_server("My Internxt"), dropbox],
            &[("My Internxt", "pw")],
            "internxt-space",
        );
        assert!(
            conf.contains("rclone config reconnect \"My Internxt:\""),
            "{conf}"
        );
        assert!(
            conf.contains("rclone config reconnect \"My Dropbox:\""),
            "{conf}"
        );
        assert!(!conf.contains("reconnect My "), "{conf}");
    }

    /// rclone keeps a comment with the section that FOLLOWS it: a comment
    /// left inside a section's body moves under the next remote the first
    /// time rclone rewrites the file (measured on rclone v1.75.1 with
    /// `rclone config update`). Every note about a remote (sign-in steps,
    /// missing credentials, a refused or password-less crypt overlay) therefore
    /// sits above that remote's header, and no body carries one.
    #[test]
    fn test_export_rclone_guidance_sits_above_its_own_section() {
        let servers = vec![
            internxt_server("internxt-acct"),
            export_server("drop", "dropbox", None),
            RcloneExportServer {
                username: "me@example.com".to_string(),
                ..export_server("filen-acct", "filen", None)
            },
            export_server("plain-ftp", "ftp", None),
            // A crypt overlay with no password: its own guidance.
            export_server(
                "sealed",
                "ftp",
                Some(serde_json::json!({ "rcloneCryptEnabled": true })),
            ),
            // A crypt overlay whose name is taken: refused, noted on its base.
            export_server(
                "nas",
                "ftp",
                Some(serde_json::json!({
                    "rcloneCryptEnabled": true,
                    "rcloneCryptOverlayName": "vault",
                    "rcloneCryptPassword": "pw",
                })),
            ),
            export_server("vault", "ftp", None),
        ];
        let (outcome, conf) = export_with_passwords(
            &servers,
            &[
                ("internxt-acct", "pw"),
                ("filen-acct", "pw"),
                ("plain-ftp", "pw"),
                ("sealed", "pw"),
                ("nas", "pw"),
                ("vault", "pw"),
            ],
            "guidance-above",
        );
        // Seven base remotes and the one crypt overlay that has a free name.
        assert_eq!(outcome.exported, 8, "{conf}");
        for (name, needle) in [
            (
                "internxt-acct",
                "rclone config reconnect \"internxt-acct:\"",
            ),
            ("drop", "rclone config reconnect \"drop:\""),
            ("filen-acct", "api_key required but unavailable"),
            ("sealed-crypt", "password required but unavailable"),
            ("nas", "crypt overlay for 'nas' not written"),
        ] {
            let (notes, body) = notes_and_body(&conf, name);
            assert!(
                notes.join("\n").contains(needle),
                "guidance for {name} is not directly above it:\n{conf}"
            );
            assert!(
                !body.iter().any(|l| l.starts_with('#')),
                "a comment inside {name}'s body would move on rewrite:\n{conf}"
            );
        }
        for name in ["plain-ftp", "sealed", "vault"] {
            let (notes, body) = notes_and_body(&conf, name);
            assert!(notes.is_empty(), "no stray guidance above {name}:\n{conf}");
            assert!(!body.iter().any(|l| l.starts_with('#')), "{conf}");
        }
    }

    /// When no name is left for an alias the export invents (a start folder,
    /// a pinned S3 bucket), the note saying so used to trail the remote's
    /// body, where rclone's next rewrite hands it to the following section.
    #[test]
    fn test_export_rclone_omitted_alias_note_sits_above_its_remote() {
        let mut servers = vec![
            export_server(
                "box",
                "sftp",
                Some(serde_json::json!({ "initial_path": "/data" })),
            ),
            export_server("store", "s3", Some(serde_json::json!({ "bucket": "b" }))),
        ];
        // Every name each alias could take is a real profile name, which a
        // generated name always yields to.
        for base in ["box-path", "store-b"] {
            servers.push(export_server(base, "ftp", None));
            for suffix in 2..=MAX_NAME_SUFFIX {
                servers.push(export_server(&format!("{base}-{suffix}"), "ftp", None));
            }
        }
        let (_, conf) = export_to_string(&servers, "alias-omitted");
        for (name, needle) in [
            ("box", "start-folder alias for 'box' omitted"),
            ("store", "bucket alias for 'store' omitted"),
        ] {
            let (notes, body) = notes_and_body(&conf, name);
            assert!(
                notes.join("\n").contains(needle),
                "'{needle}' is not directly above [{name}]"
            );
            assert!(!body.iter().any(|l| l.starts_with('#')), "[{name}] body");
        }
    }

    /// The `remote =` of every crypt section, in order.
    fn crypt_remotes(conf: &str) -> Vec<&str> {
        let lines: Vec<&str> = conf.lines().collect();
        lines
            .windows(2)
            .filter(|pair| pair[0] == "type = crypt")
            .filter_map(|pair| pair[1].strip_prefix("remote = "))
            .collect()
    }

    /// A crypt overlay's scope is relative to the folder its alias points at
    /// (the start folder, a pinned bucket). With that alias left out, the
    /// crypt wrapped the clear remote's root: `/vault` of the account, outside
    /// the folder. It wraps the clear remote at that folder, and is refused
    /// when rclone's config cannot hold the folder.
    #[test]
    fn test_export_rclone_crypt_wraps_the_folder_whose_alias_is_left_out() {
        let crypt = |initial: Option<&str>, scope: &str| {
            let mut options = serde_json::json!({
                "rcloneCryptEnabled": true,
                "rcloneCryptPassword": "pw",
                "rcloneCryptOverlayScope": scope,
            });
            if let Some(initial) = initial {
                options["initial_path"] = initial.into();
            }
            options
        };
        // A start folder that ends in a space: no alias can hold it.
        let servers = vec![
            export_server("nas", "sftp", Some(crypt(Some("/mnt/data "), "/vault"))),
            export_server("pub", "ftp", Some(crypt(Some("/pub "), "vault"))),
            export_server("bare", "sftp", Some(crypt(Some("/mnt/data "), ""))),
        ];
        let (outcome, conf) = export_to_string(&servers, "crypt-unaliased-space");
        assert_eq!(
            crypt_remotes(&conf),
            ["nas:/mnt/data /vault", "pub:pub /vault"],
            "{conf}"
        );
        // The whole folder: `remote = bare:/mnt/data ` would open `/mnt/data`.
        let refused: Vec<(&str, &str)> = outcome
            .skipped
            .iter()
            .map(|skip| (skip.name.as_str(), skip.reason.as_str()))
            .collect();
        assert_eq!(
            refused,
            [(
                "bare",
                "it would wrap 'bare:/mnt/data ', whose name ends in a space, which rclone's config cannot hold"
            )]
        );
        let (notes, _) = notes_and_body(&conf, "bare");
        assert!(
            notes
                .join("\n")
                .contains("# crypt overlay for 'bare' not written"),
            "{conf}"
        );

        // No free name for the alias: the folder is written as it is.
        let mut servers = vec![
            export_server("box", "sftp", Some(crypt(Some("/data"), "/vault"))),
            export_server("store", "s3", Some(crypt(None, "/vault"))),
        ];
        servers[1].options.as_mut().unwrap()["bucket"] = "b".into();
        for base in ["box-path", "store-b"] {
            servers.push(export_server(base, "ftp", None));
            for suffix in 2..=MAX_NAME_SUFFIX {
                servers.push(export_server(&format!("{base}-{suffix}"), "ftp", None));
            }
        }
        let (outcome, conf) = export_to_string(&servers, "crypt-unaliased-taken");
        assert_eq!(crypt_remotes(&conf), ["box:/data/vault", "store:b/vault"]);
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    }

    #[test]
    fn test_import_rclone_internxt_reveals_real_rclone_obscured() {
        // `pass` below was produced by the real rclone binary (`rclone obscure
        // TestPass123`, rclone v1.75.1), so this pins the reveal codec to
        // rclone's actual output for the `internxt` backend's IsPassword field.
        // The second remote has no email, which AeroFTP cannot sign in without,
        // and the import says so instead of calling the type unsupported.
        let conf = "\
[internxt-real]
type = internxt
email = real@example.com
pass = ANMkm3ZpMPvnz_0z5dZ-68G17MaOiI2s3wiL

[internxt-no-email]
type = internxt
pass = ANMkm3ZpMPvnz_0z5dZ-68G17MaOiI2s3wiL
";
        let path = tmp_write(
            conf,
            &format!(
                "aeroftp-test-import-internxt-real-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let internxt: Vec<_> = result
            .servers
            .iter()
            .filter(|s| s.protocol.as_deref() == Some("internxt"))
            .collect();
        assert_eq!(internxt.len(), 1, "only the remote with an email imports");
        assert_eq!(internxt[0].username, "real@example.com");
        assert_eq!(internxt[0].credential.as_deref(), Some("TestPass123"));
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].name, "internxt-no-email");
        assert_eq!(result.skipped[0].reason, "internxt remote has no email");
    }

    /// Every secret the importer reveals into `options` is named in
    /// `IMPORT_SECRET_OPTION_KEYS`, checked on the values rather than the key
    /// names: the crypt password and salt and a Filen API key go in revealed,
    /// and none of them survives `options_without_secrets`.
    #[test]
    fn test_import_secret_option_keys_cover_every_revealed_secret() {
        let secrets = ["CryptPassPlain1", "CryptSaltPlain2", "FilenApiKeyPlain3"];
        let obscured: Vec<String> = secrets
            .iter()
            .map(|s| obscure_password(s).expect("obscure"))
            .collect();
        let conf = format!(
            "\
[base]
type = sftp
host = example.com
user = me

[vault]
type = crypt
remote = base:vault
password = {}
password2 = {}

[filen-acct]
type = filen
email = me@example.com
password = {}
api_key = {}
",
            obscured[0], obscured[1], obscured[0], obscured[2]
        );
        let path = tmp_write(
            &conf,
            &format!(
                "aeroftp-test-import-secret-options-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let raw = serde_json::to_string(
            &result
                .servers
                .iter()
                .map(|s| s.options.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for secret in secrets {
            assert!(
                raw.contains(secret),
                "the fixture must reach '{secret}':\n{raw}"
            );
        }
        let shown = serde_json::to_string(
            &result
                .servers
                .iter()
                .map(|s| options_without_secrets(s.options.as_ref()))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for secret in secrets {
            assert!(!shown.contains(secret), "'{secret}' survives:\n{shown}");
        }
    }

    /// A crypt `remote =` may be a connection string, an on-the-fly backend or
    /// a local path, whose parameters can be secrets. None of it is imported
    /// and the reason never repeats a parameter; it used to quote the whole
    /// base, `mys3,secret_access_key=...`, and call it missing from the file.
    #[test]
    fn test_import_rclone_crypt_reason_never_repeats_connection_parameters() {
        let conf = "\
[mys3]
type = s3
provider = AWS
region = eu-west-1
access_key_id = AKIAEXAMPLE

[vault-params]
type = crypt
remote = mys3,secret_access_key=TopSecretValue42:bucket

[vault-onthefly]
type = crypt
remote = :s3,access_key_id=AKIAOTHER,secret_access_key=OtherSecret99:bucket

[vault-local]
type = crypt
remote = /home/me/private-vault
";
        let path = tmp_write(
            conf,
            &format!(
                "aeroftp-test-import-crypt-params-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let reason = |name: &str| {
            result
                .skipped
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.reason.clone())
                .unwrap_or_else(|| panic!("{name} not skipped"))
        };
        assert_eq!(
            reason("vault-params"),
            "crypt remote wraps 'mys3' with connection-string parameters, which AeroFTP cannot carry"
        );
        let all = format!(
            "{:?}",
            result.skipped.iter().map(|s| &s.reason).collect::<Vec<_>>()
        );
        for leaked in [
            "TopSecretValue42",
            "OtherSecret99",
            "AKIAOTHER",
            "secret_access_key",
            "private-vault",
        ] {
            assert!(!all.contains(leaked), "'{leaked}' in a skip reason: {all}");
        }
        assert_eq!(
            reason("vault-onthefly"),
            "crypt remote wraps an on-the-fly backend, which AeroFTP cannot carry"
        );
        assert!(
            reason("vault-local").contains("not a named remote"),
            "{}",
            reason("vault-local")
        );
        assert!(
            !result.servers.iter().any(|s| s.name.starts_with("vault")),
            "no crypt imported over a base it cannot describe"
        );
    }

    /// A remote that cannot import says why. Every missing required field
    /// used to come back as "unsupported rclone type", which sends the reader
    /// looking for a gap in AeroFTP instead of in the file.
    #[test]
    fn test_import_rclone_skip_reasons_name_what_is_missing() {
        let conf = "\
[ftp-no-host]
type = ftp
user = u

[crypt-orphan]
type = crypt
remote = nowhere:vault

[crypt-on-broken]
type = crypt
remote = ftp-no-host:vault

[crypt-empty]
type = crypt

[odd]
type = definitely-not-a-backend
";
        let path = tmp_write(
            conf,
            &format!(
                "aeroftp-test-import-skip-reasons-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let reason = |name: &str| {
            result
                .skipped
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.reason.clone())
                .unwrap_or_else(|| panic!("{name} not skipped: {:?}", result.skipped.len()))
        };
        assert_eq!(reason("ftp-no-host"), "ftp remote has no host");
        assert_eq!(
            reason("crypt-orphan"),
            "crypt remote wraps 'nowhere', which is not in this file"
        );
        assert_eq!(
            reason("crypt-on-broken"),
            "crypt remote wraps 'ftp-no-host': ftp remote has no host"
        );
        assert_eq!(reason("crypt-empty"), "crypt remote has no remote to wrap");
        assert_eq!(
            reason("odd"),
            "unsupported rclone type: definitely-not-a-backend"
        );
        assert!(result.servers.is_empty());
    }

    /// `pass` is an rclone IsPassword field: rclone writes it obscured and
    /// cannot use one that does not reveal. Such a value used to be kept as
    /// the password itself; now the profile imports without one, and says so.
    #[test]
    fn test_import_rclone_internxt_unrevealable_pass_imports_without_it() {
        // Neither is what rclone's Reveal accepts. The first is plain text.
        // The second is the real `rclone obscure TestPass123` output with the
        // URL-safe alphabet swapped for the standard one (`-` to `+`, `_` to
        // `/`): rclone refuses it, while the lenient reveal used elsewhere
        // retries standard base64 and would import it as "TestPass123".
        for bad in ["S3cr3tPass!", "ANMkm3ZpMPvnz/0z5dZ+68G17MaOiI2s3wiL"] {
            let conf =
                format!("[internxt-bad]\ntype = internxt\nemail = me@example.com\npass = {bad}\n");
            let path = tmp_write(
                &conf,
                &format!(
                    "aeroftp-test-import-internxt-bad-{}.conf",
                    std::process::id()
                ),
            );
            let result = import_rclone(&path).unwrap();
            std::fs::remove_file(&path).ok();
            let internxt = result
                .servers
                .iter()
                .find(|s| s.protocol.as_deref() == Some("internxt"))
                .expect("the profile still imports");
            assert_eq!(internxt.credential, None, "'{bad}' is not a password");
            assert_eq!(result.warnings.len(), 1, "'{bad}': no warning");
            assert_eq!(result.warnings[0].name, "internxt-bad");
            let reason = &result.warnings[0].reason;
            assert!(
                reason.starts_with("pass does not reveal")
                    && reason.contains("imported without it"),
                "{reason}"
            );
            // The decoder names the offending symbol and its offset: for a
            // plaintext password that is a piece of it, and this goes to logs.
            assert!(
                !reason.contains("symbol") && !reason.contains("offset"),
                "the reason leaks a piece of the value: {reason}"
            );
        }
    }

    /// After `rclone config reconnect`, rclone's internxt Config stores the
    /// obscured `mnemonic` and an OAuth-shaped `token` in the section. AeroFTP
    /// derives both at every sign-in, so neither may leak into the profile.
    #[test]
    fn test_import_rclone_internxt_ignores_mnemonic_and_token() {
        let conf = "\
[internxt-after-reconnect]
type = internxt
email = real@example.com
pass = ANMkm3ZpMPvnz_0z5dZ-68G17MaOiI2s3wiL
mnemonic = ANMkm3ZpMPvnz_0z5dZ-68G17MaOiI2s3wiL
token = {\"access_token\":\"eyJhbGciOi.x.y\",\"token_type\":\"Bearer\",\"expiry\":\"2026-10-04T00:00:00Z\"}
";
        let path = tmp_write(
            conf,
            &format!(
                "aeroftp-test-import-internxt-reconnected-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let internxt = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("internxt"))
            .expect("internxt server present");
        assert_eq!(internxt.credential.as_deref(), Some("TestPass123"));
        assert_eq!(internxt.options, None, "no mnemonic or token in options");
        assert!(result.provider_secrets.is_empty(), "no token imported");
        assert!(result.warnings.is_empty());
    }

    fn api_key_server(
        name: &str,
        protocol: &str,
        host: &str,
        username: &str,
        options: Option<serde_json::Value>,
    ) -> RcloneExportServer {
        RcloneExportServer {
            id: name.to_string(),
            name: name.to_string(),
            host: host.to_string(),
            port: 443,
            username: username.to_string(),
            protocol: Some(protocol.to_string()),
            options,
            provider_id: Some(protocol.to_string()),
        }
    }

    /// Drime, Cloudinary and ImageKit used to be refused as "not
    /// exportable". Their rclone fields are not `IsPassword` (rclone v1.75.1),
    /// so the keys are written exactly as AeroFTP holds them: rclone sends
    /// these fields as they are, and an obscured one is a wrong key.
    #[test]
    fn test_export_rclone_api_key_providers_write_their_keys_plain() {
        let servers = vec![
            api_key_server("dr", "drime", "app.drime.cloud", "api-token", None),
            api_key_server(
                "cl",
                "cloudinary",
                "api.cloudinary.com",
                "599944212611461",
                Some(serde_json::json!({ "bucket": "demo-cloud" })),
            ),
            api_key_server(
                "ik",
                "imagekit",
                "api.imagekit.io",
                "demo_id",
                Some(serde_json::json!({ "imagekit_public_key": "public_abc=" })),
            ),
            // The dashboard URL pasted whole, trailing slash included.
            api_key_server(
                "ik-url",
                "imagekit",
                "api.imagekit.io",
                "https://ik.imagekit.io/other_id/",
                Some(serde_json::json!({ "imagekit_public_key": "public_def=" })),
            ),
        ];
        let passwords: HashMap<String, String> = [
            ("dr", "12|drimeTokenValue"),
            ("cl", "hwPq3vxxVum2xqOoHU7bJ6Cqnp8"),
            ("ik", "private_abc="),
            ("ik-url", "private_def="),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-test-export-api-key-providers-{}.conf",
            std::process::id()
        ));
        let outcome = export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(outcome.exported, 4, "{conf}");
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
        for section in [
            "[dr]\ntype = drime\naccess_token = 12|drimeTokenValue\n",
            "[cl]\ntype = cloudinary\ncloud_name = demo-cloud\napi_key = 599944212611461\n\
             api_secret = hwPq3vxxVum2xqOoHU7bJ6Cqnp8\n",
            "[ik]\ntype = imagekit\nendpoint = https://ik.imagekit.io/demo_id\n\
             public_key = public_abc=\nprivate_key = private_abc=\n",
            "[ik-url]\ntype = imagekit\nendpoint = https://ik.imagekit.io/other_id\n\
             public_key = public_def=\nprivate_key = private_def=\n",
        ] {
            assert!(conf.contains(section), "missing:\n{section}\nin:\n{conf}");
        }
    }

    /// Written by the real rclone binary (`rclone config create`, v1.75.1),
    /// which stores all three backends' keys plain. The keys decode as
    /// obscured values, so a reveal anywhere on this path would change them.
    #[test]
    fn test_import_rclone_api_key_providers_from_real_rclone_config() {
        let conf = "\
[dr]
type = drime
access_token = jux3pFx5rDYgmXi0rjy5CWhBCyCRMiJMswW7zVQl

[cl]
type = cloudinary
cloud_name = demo-cloud
api_key = 599944212611461
api_secret = hwPq3vxxVum2xqOoHU7bJ6Cqnp8

[ik]
type = imagekit
endpoint = https://ik.imagekit.io/demo_id
public_key = public_Xy12AbCdEfGhIjKlMnOpQrStU=
private_key = private_PaLkUDUPvkI4w8tUjcdPfljtLGJv

[lu]
type = filelu
key = RC_abcdefghijklmnopqrst
";
        for key in [
            "jux3pFx5rDYgmXi0rjy5CWhBCyCRMiJMswW7zVQl",
            "hwPq3vxxVum2xqOoHU7bJ6Cqnp8",
            "private_PaLkUDUPvkI4w8tUjcdPfljtLGJv",
        ] {
            assert!(
                matches!(reveal_obscured(key), Ok(ref r) if !r.is_empty()),
                "{key} no longer decodes, so it no longer tests anything"
            );
        }
        let path = tmp_write(
            conf,
            &format!(
                "aeroftp-test-import-api-key-real-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let server = |name: &str| {
            result
                .servers
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} not imported"))
        };

        let dr = server("dr");
        assert_eq!(dr.protocol.as_deref(), Some("drime"));
        assert_eq!(dr.host, "app.drime.cloud");
        assert_eq!(dr.username, "api-token");
        assert_eq!(
            dr.credential.as_deref(),
            Some("jux3pFx5rDYgmXi0rjy5CWhBCyCRMiJMswW7zVQl")
        );

        let cl = server("cl");
        assert_eq!(cl.protocol.as_deref(), Some("cloudinary"));
        assert_eq!(cl.host, "api.cloudinary.com");
        assert_eq!(cl.username, "599944212611461");
        assert_eq!(
            cl.credential.as_deref(),
            Some("hwPq3vxxVum2xqOoHU7bJ6Cqnp8")
        );
        assert_eq!(
            cl.options,
            Some(serde_json::json!({ "bucket": "demo-cloud" }))
        );

        let ik = server("ik");
        assert_eq!(ik.protocol.as_deref(), Some("imagekit"));
        assert_eq!(ik.host, "api.imagekit.io");
        assert_eq!(ik.username, "https://ik.imagekit.io/demo_id");
        assert_eq!(
            ik.credential.as_deref(),
            Some("private_PaLkUDUPvkI4w8tUjcdPfljtLGJv")
        );
        assert_eq!(
            ik.options,
            Some(serde_json::json!({ "imagekit_public_key": "public_Xy12AbCdEfGhIjKlMnOpQrStU=" }))
        );

        let lu = result
            .skipped
            .iter()
            .find(|s| s.name == "lu")
            .expect("the FileLu remote is skipped");
        assert!(lu.reason.contains("Rclone key"), "{}", lu.reason);
        assert!(result.warnings.is_empty());
    }

    /// What AeroFTP exports for Drime, Cloudinary and ImageKit imports back as
    /// the same profile, public key included.
    #[test]
    fn test_export_rclone_api_key_providers_roundtrip() {
        let servers = vec![
            api_key_server("dr", "drime", "app.drime.cloud", "api-token", None),
            api_key_server(
                "cl",
                "cloudinary",
                "api.cloudinary.com",
                "599944212611461",
                Some(serde_json::json!({ "bucket": "demo-cloud" })),
            ),
            api_key_server(
                "ik",
                "imagekit",
                "api.imagekit.io",
                "https://ik.imagekit.io/demo_id",
                Some(serde_json::json!({ "imagekit_public_key": "public_abc=" })),
            ),
        ];
        let passwords: HashMap<String, String> = [
            ("dr", "12|drimeTokenValue"),
            ("cl", "hwPq3vxxVum2xqOoHU7bJ6Cqnp8"),
            ("ik", "private_abc="),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-test-roundtrip-api-key-{}.conf",
            std::process::id()
        ));
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let result = import_rclone(&tmp).unwrap();
        std::fs::remove_file(&tmp).ok();

        assert_eq!(
            result.servers.len(),
            3,
            "skipped: {:?}",
            result.skipped.len()
        );
        for original in &servers {
            let back = result
                .servers
                .iter()
                .find(|s| s.name == original.name)
                .unwrap_or_else(|| panic!("{} did not come back", original.name));
            assert_eq!(back.protocol, original.protocol, "{}", original.name);
            assert_eq!(back.host, original.host, "{}", original.name);
            assert_eq!(back.username, original.username, "{}", original.name);
            assert_eq!(back.options, original.options, "{}", original.name);
            assert_eq!(
                back.credential.as_deref(),
                passwords.get(&original.name).map(String::as_str),
                "{}",
                original.name
            );
        }
    }

    /// Remotes AeroFTP would open onto other content, or cannot sign in to,
    /// are skipped with the reason instead of imported.
    #[test]
    fn test_import_rclone_api_key_providers_refuse_what_aeroftp_cannot_open() {
        let conf = "\
[dr-ws]
type = drime
access_token = token
workspace_id = 42

[dr-root]
type = drime
access_token = token
root_folder_id = 1234

[dr-empty]
type = drime

[cl-eu]
type = cloudinary
cloud_name = demo-cloud
api_key = 1
api_secret = s
upload_prefix = https://api-eu.cloudinary.com

[cl-nameless]
type = cloudinary
api_key = 1
api_secret = s

[ik-no-endpoint]
type = imagekit
public_key = p
private_key = k
";
        let path = tmp_write(
            conf,
            &format!(
                "aeroftp-test-import-api-key-refused-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(result.servers.is_empty(), "{:?}", result.servers.len());
        for (name, needle) in [
            ("dr-ws", "workspace 42"),
            ("dr-root", "folder id 1234"),
            ("dr-empty", "no access_token"),
            ("cl-eu", "api-eu.cloudinary.com"),
            ("cl-nameless", "no cloud_name"),
            ("ik-no-endpoint", "no endpoint"),
        ] {
            let skipped = result
                .skipped
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} not reported"));
            assert!(
                skipped.reason.contains(needle),
                "{name}: {}",
                skipped.reason
            );
        }
    }

    /// A Cloudinary profile without its cloud name and an ImageKit profile
    /// without the public key would be remotes rclone cannot use (ImageKit's
    /// it refuses to create), so neither is written and each says why.
    #[test]
    fn test_export_rclone_skips_api_key_providers_missing_a_required_field() {
        let servers = vec![
            api_key_server(
                "cl",
                "cloudinary",
                "api.cloudinary.com",
                "599944212611461",
                None,
            ),
            api_key_server("ik", "imagekit", "api.imagekit.io", "demo_id", None),
            api_key_server("dr", "drime", "app.drime.cloud", "api-token", None),
        ];
        let (outcome, conf) = export_to_string(&servers, "api-key-missing-field");

        assert_eq!(outcome.exported, 1, "only Drime:\n{conf}");
        assert!(!conf.contains("[cl]"), "{conf}");
        assert!(!conf.contains("[ik]"), "{conf}");
        let reason = |name: &str| {
            outcome
                .skipped
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.reason.clone())
                .unwrap_or_else(|| panic!("{name} not reported: {:?}", outcome.skipped))
        };
        assert!(reason("cl").contains("cloud name"), "{}", reason("cl"));
        assert!(reason("ik").contains("public key"), "{}", reason("ik"));
    }

    #[test]
    fn test_export_rclone_filen_without_api_key_emits_guidance_comment() {
        // #128-D: rclone's `filen` backend marks `api_key` Required and cannot
        // derive it from the password. A Filen profile saved with only
        // email+password (no Filen CLI API Key) must export `type = filen` +
        // email + password PLUS a guidance comment, NEVER a half-remote with a
        // missing api_key that fails at use with "input too short".
        let servers = vec![RcloneExportServer {
            id: "filen-noapikey".to_string(),
            name: "filen-noapikey".to_string(),
            host: "filen.io".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("filen".to_string()),
            options: None,
            provider_id: Some("filen".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("filen-noapikey".to_string(), "S3cr3tPass!".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-filen-noapikey.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(conf.contains("type = filen"), "missing type:\n{conf}");
        assert!(
            conf.contains("email = me@example.com"),
            "missing email:\n{conf}"
        );
        assert!(
            conf.contains("password = ") && !conf.contains("password = S3cr3tPass!"),
            "password must be present and obscured:\n{conf}"
        );
        // No api_key value line, but a guidance comment must appear.
        let has_real_api_key_line = conf
            .lines()
            .any(|l| l.trim_start().starts_with("api_key ="));
        assert!(
            !has_real_api_key_line,
            "must NOT emit a broken api_key line:\n{conf}"
        );
        assert!(
            conf.contains("# api_key required but unavailable"),
            "must emit the api_key guidance comment:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_onedrive_emits_drive_id_and_type() {
        // #128-D: rclone's `onedrive` backend needs `drive_id` + `drive_type`
        // (it fails at use with "unable to get drive_id and drive_type"). AeroFTP
        // captures both at connect and the bridge injects them into options; the
        // export arm must emit them verbatim.
        let servers = vec![RcloneExportServer {
            id: "onedrive-acct".to_string(),
            name: "onedrive-acct".to_string(),
            host: "graph.microsoft.com".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("onedrive".to_string()),
            options: Some(serde_json::json!({
                "drive_id": "D980DD4FB1784A97",
                "drive_type": "personal",
                "__aeroftp_oauth_token": "{\"access_token\":\"tok\",\"token_type\":\"Bearer\"}",
                "__aeroftp_oauth_client_id": "cid",
                "__aeroftp_oauth_client_secret": "csec",
            })),
            provider_id: Some("onedrive".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-export-onedrive.conf");
        export_rclone(&servers, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(conf.contains("type = onedrive"), "type:\n{conf}");
        assert!(
            conf.contains("drive_id = D980DD4FB1784A97"),
            "must emit drive_id:\n{conf}"
        );
        assert!(
            conf.contains("drive_type = personal"),
            "must emit drive_type:\n{conf}"
        );
    }

    #[test]
    fn test_import_rclone_filen_reveals_real_rclone_obscured() {
        // The obscured values below were produced by the real rclone binary
        // (`rclone obscure`), so this pins our reveal codec to rclone's actual
        // output for the `filen` backend's IsPassword fields.
        let conf = "\
[filen-real]
type = filen
email = real@example.com
password = MohwAn6swmCQmBhRD-iaWciNBrBXM-ph2axM
api_key = 4BVmu-SCRQai2-0-hucKgbeyzH6-uqexma-skpRs4Kk
";
        let path = tmp_write(conf, "aeroftp-test-import-filen-real.conf");
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let filen = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("filen"))
            .expect("filen server present");
        assert_eq!(filen.username, "real@example.com");
        assert_eq!(filen.credential.as_deref(), Some("TestPass123"));
        assert_eq!(
            filen
                .options
                .as_ref()
                .and_then(|o| o.get("filen_api_key"))
                .and_then(|v| v.as_str()),
            Some("fake-api-key-abc"),
        );
    }

    #[test]
    fn test_export_rclone_backblaze_key_is_plaintext() {
        // AeroFTP's native Backblaze protocol exports to rclone's `b2` backend.
        // `key` is NOT an rclone IsPassword field, so it must be emitted plain
        // (verified against `rclone config providers`: b2.key IsPassword=false).
        let servers = vec![RcloneExportServer {
            id: "b2-acct".to_string(),
            name: "b2-acct".to_string(),
            host: "api.backblazeb2.com".to_string(),
            port: 443,
            username: "0011deadbeef".to_string(),
            protocol: Some("backblaze".to_string()),
            options: Some(serde_json::json!({ "bucket": "my-bucket" })),
            provider_id: Some("backblaze-native".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("b2-acct".to_string(), "K001abcdEFG키".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-b2.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(conf.contains("type = b2"), "missing type:\n{conf}");
        assert!(
            conf.contains("account = 0011deadbeef"),
            "missing account:\n{conf}"
        );
        assert!(
            conf.contains("key = K001abcdEFG키"),
            "b2 key must be emitted plain, not obscured:\n{conf}"
        );
    }

    /// rclone writes these secrets plain (none is `IsPassword` in v1.75.1), so
    /// the import must store them as they are. They used to go through the
    /// reveal codec, and a value that happens to decode came back as a few
    /// bytes of noise, stored with no error or warning. Among random keys of
    /// each shape, that is 0.03% of AWS secret keys and 0.4% of B2
    /// application keys. The values below are such keys. Real 88-character
    /// Azure keys almost never decode, but the field was read the same way,
    /// so a shorter padded one stands in.
    #[test]
    fn test_import_rclone_reads_plain_secrets_verbatim() {
        let aws = "ouaVyjxSEguS+NiX/v56YGxMK4/J/lodDY+S/On5";
        let azure = "iUSqbsCq1b4r8llxjuIJDCkAT8YhG3eK+hUtI5D113g=";
        let swift = "0M5gBRXoIPaqWVKLxLethR6aS2Nlr0nz";
        let b2 = "K005ACpVVct0jQ2rTOZSGY64zPV3rB8";
        for value in [aws, azure, swift, b2] {
            assert!(
                matches!(reveal_obscured(value), Ok(ref r) if !r.is_empty()),
                "{value} no longer decodes, so it no longer tests anything"
            );
        }
        let conf = format!(
            "\
[aws]
type = s3
provider = AWS
access_key_id = AKIAEXAMPLE
secret_access_key = {aws}

[az]
type = azureblob
account = demoaccount
key = {azure}

[sw]
type = swift
auth = https://auth.example.com/v3
user = demo
key = {swift}

[b2]
type = b2
account = 0051234567890ab0000000001
key = {b2}
"
        );
        let path = tmp_write(
            &conf,
            &format!(
                "aeroftp-test-import-plain-secrets-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        for (name, expected) in [("aws", aws), ("az", azure), ("sw", swift), ("b2", b2)] {
            let server = result
                .servers
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} not imported: {:?}", result.skipped.len()));
            assert_eq!(server.credential.as_deref(), Some(expected), "{name}");
        }
    }

    /// Every field rclone marks `IsPassword` (v1.75.1) is revealed the way
    /// rclone reveals it. A value rclone could not reveal used to be stored as
    /// the secret itself, or, in the standard alphabet, revealed anyway: now
    /// the profile imports without it and the import report says which field.
    /// A value rclone did obscure still reveals, in every one of them.
    #[test]
    fn test_import_rclone_reveals_every_password_field_strictly() {
        // `rclone obscure TestPass123` (rclone v1.75.1), and the same bytes in
        // the standard base64 alphabet, which rclone's Reveal refuses.
        let good = "ANMkm3ZpMPvnz_0z5dZ-68G17MaOiI2s3wiL";
        let std_alphabet = "ANMkm3ZpMPvnz/0z5dZ+68G17MaOiI2s3wiL";
        let remotes = |value: &str| {
            format!(
                "\
[ftp]
type = ftp
host = ftp.example.com
user = demo
pass = {value}

[sftp]
type = sftp
host = sftp.example.com
user = demo
pass = {value}

[dav]
type = webdav
url = https://dav.example.com/remote.php/dav/files/demo
user = demo
pass = {value}

[mega]
type = mega
user = me@example.com
pass = {value}

[filen]
type = filen
email = me@example.com
password = {value}
api_key = {value}

[koofr]
type = koofr
user = me@example.com
password = {value}

[opendrive]
type = opendrive
username = me@example.com
password = {value}

[vault]
type = crypt
remote = ftp:/vault
password = {value}
password2 = {value}
"
            )
        };
        let import = |conf: String, tag: &str| {
            let path = tmp_write(
                &conf,
                &format!(
                    "aeroftp-test-import-strict-{tag}-{}.conf",
                    std::process::id()
                ),
            );
            let result = import_rclone(&path).unwrap();
            std::fs::remove_file(&path).ok();
            result
        };
        let secrets = |s: &ServerProfileExport| -> Vec<Option<String>> {
            let opt = |k: &str| {
                s.options
                    .as_ref()
                    .and_then(|o| o.get(k))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            };
            match s.name.as_str() {
                "filen" => vec![s.credential.clone(), opt("filen_api_key")],
                "vault" => vec![opt("rcloneCryptPassword"), opt("rcloneCryptPassword2")],
                _ => vec![s.credential.clone()],
            }
        };
        let names = [
            "ftp",
            "sftp",
            "dav",
            "mega",
            "filen",
            "koofr",
            "opendrive",
            "vault",
        ];

        let result = import(remotes(good), "good");
        assert!(result.warnings.is_empty(), "{:?}", result.warnings.len());
        for name in names {
            let server = result.servers.iter().find(|s| s.name == name).expect(name);
            for secret in secrets(server) {
                assert_eq!(secret.as_deref(), Some("TestPass123"), "{name}");
            }
        }

        for (tag, bad) in [("plain", "S3cr3tPass!"), ("std", std_alphabet)] {
            let result = import(remotes(bad), tag);
            // A crypt remote with an unreadable password is not imported at
            // all: an overlay without its key would decrypt nothing.
            let vault = result
                .skipped
                .iter()
                .find(|s| s.name == "vault")
                .unwrap_or_else(|| panic!("vault still imports ('{bad}')"));
            assert!(vault.reason.contains("password "), "{}", vault.reason);
            for name in names.into_iter().filter(|n| *n != "vault") {
                let server = result
                    .servers
                    .iter()
                    .find(|s| s.name == name)
                    .unwrap_or_else(|| panic!("{name} still imports ('{bad}')"));
                for secret in secrets(server) {
                    assert_eq!(secret, None, "{name}: '{bad}' is not a password");
                }
                let warning = result
                    .warnings
                    .iter()
                    .find(|w| w.name == name)
                    .unwrap_or_else(|| panic!("{name}: no warning for '{bad}'"));
                assert!(
                    warning.reason.contains("imported without"),
                    "{name}: {}",
                    warning.reason
                );
            }
            let filen = result.warnings.iter().find(|w| w.name == "filen").unwrap();
            assert!(
                filen.reason.contains("password") && filen.reason.contains("api_key"),
                "both filen fields are named: {}",
                filen.reason
            );
        }
    }

    /// AeroFTP's own export wrote S3 `secret_access_key` and the Azure `key`
    /// obscured from its first rclone export (v3.4.7) until v3.7.6, and the
    /// Swift `key` until v4.0.5, although rclone keeps those fields plain. A
    /// file from those versions says so in its header, and its keys are the
    /// obscured ones: they must still import as the keys. The values below are
    /// `rclone obscure` output (v1.75.1) for the keys in the assertions, and
    /// the layout is the one v3.7.4 wrote.
    #[test]
    fn test_import_rclone_reveals_keys_an_older_aeroftp_export_obscured() {
        let aws = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let aws_obscured =
            "bgeHgvsu5PV96aZcpunIFC5EDvmaaHeNaaE7wuo2T7_GycW7y1zEI7a6f5iYPOi7Ropnc73qzcw";
        let azure = "Eby8vdM02xNOcqFLqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";
        let azure_obscured = "1aRH1kFnoBoRaUGK3K355gqn9cNXBgPNPS6nv13pJxOiiWO_ifHHQSDj8L3PLoGSjbs56nhq2oWmBu7NuQiHw09ECJsr_B6xh6CamylBVCinNhd4BMZKCnYVdnGZmmSqcODKPg8Lmfk";
        let swift = "148%BlomPass";
        let swift_obscured = "mTBDHNVmUspfrftDRi3tmN1Seri-0wMt2o6lKQ";
        let file = |header: &str| {
            format!(
                "{header}\
[aws]
type = s3
provider = AWS
access_key_id = AKIAIOSFODNN7EXAMPLE
secret_access_key = {aws_obscured}
region = us-east-1

[az]
type = azureblob
account = demoaccount
key = {azure_obscured}

[sw]
type = swift
user = demo
key = {swift_obscured}
auth = https://auth.example.com/v3

"
            )
        };
        let import = |conf: String, tag: &str| {
            let path = tmp_write(
                &conf,
                &format!(
                    "aeroftp-test-import-old-export-{tag}-{}.conf",
                    std::process::id()
                ),
            );
            let result = import_rclone(&path).unwrap();
            std::fs::remove_file(&path).ok();
            result
        };
        let key_of = |result: &RcloneImportResult, name: &str| {
            result
                .servers
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} not imported"))
                .credential
                .clone()
        };
        let header = |exported: &str| {
            format!("# Generated by AeroFTP - https://aeroftp.app\n# Exported: {exported}\n\n")
        };

        // v3.7.4: all three written obscured. Each reveal is reported, since
        // a key edited into such a file later can decode too.
        let old = import(file(&header("2026-05-07T19:22:19Z")), "v374");
        assert_eq!(key_of(&old, "aws").as_deref(), Some(aws));
        assert_eq!(key_of(&old, "az").as_deref(), Some(azure));
        assert_eq!(key_of(&old, "sw").as_deref(), Some(swift));
        for (name, field) in [("aws", "secret_access_key"), ("az", "key"), ("sw", "key")] {
            let note = old
                .warnings
                .iter()
                .find(|w| w.name == name)
                .unwrap_or_else(|| panic!("{name}: the reveal is not reported"));
            assert!(
                note.reason.starts_with(&format!("{field} was revealed")),
                "{}",
                note.reason
            );
        }

        // Between v3.7.6 and v4.0.5 only the Swift key was still obscured.
        let mid = import(file(&header("2026-06-01T08:00:00Z")), "v400");
        assert_eq!(key_of(&mid, "sw").as_deref(), Some(swift));
        assert_eq!(key_of(&mid, "aws").as_deref(), Some(aws_obscured));
        let aws_note = mid
            .warnings
            .iter()
            .find(|w| w.name == "aws")
            .expect("an S3 key that reads as obscured, in a file after the fix, is flagged");
        assert!(
            aws_note.reason.contains("secret_access_key"),
            "{}",
            aws_note.reason
        );

        // A file that is not an AeroFTP export: rclone would send these values
        // as they are, and so they are kept, with no note.
        let foreign = import(file(""), "foreign");
        assert_eq!(key_of(&foreign, "aws").as_deref(), Some(aws_obscured));
        assert_eq!(key_of(&foreign, "sw").as_deref(), Some(swift_obscured));
        assert!(foreign.warnings.is_empty());
    }

    /// rclone keeps the AeroFTP header when it rewrites a file, so an old
    /// export used as rclone.conf can hold a key fixed later in plain text
    /// (`rclone config update`). The file below is what rclone v1.75.1 wrote
    /// after `rclone config update aws secret_access_key=K005...` on a v3.7.4
    /// export: the S3 key is plain, a B2-shaped key that decodes to a few
    /// bytes, and the Swift key is still the obscured one.
    #[test]
    fn test_import_rclone_keeps_a_plain_key_edited_into_an_old_aeroftp_export() {
        let conf = "\
# Generated by AeroFTP - https://aeroftp.app
# Exported: 2026-05-07T19:22:19Z
[aws]
type = s3
provider = AWS
access_key_id = AKIAIOSFODNN7EXAMPLE
secret_access_key = K00500UrPoPrwM702BIqoNhg9L3LlqY
region = us-east-1

[sw]
type = swift
user = demo
key = mTBDHNVmUspfrftDRi3tmN1Seri-0wMt2o6lKQ
auth = https://auth.example.com/v3
";
        assert!(
            matches!(reveal_rclone_password("K00500UrPoPrwM702BIqoNhg9L3LlqY"), Ok(ref r) if !r.is_empty()),
            "the edited key must still decode, or this tests nothing"
        );
        let path = tmp_write(
            conf,
            &format!("aeroftp-test-import-rewritten-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let key_of = |name: &str| {
            result
                .servers
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .credential
                .clone()
        };
        assert_eq!(
            key_of("aws").as_deref(),
            Some("K00500UrPoPrwM702BIqoNhg9L3LlqY")
        );
        assert_eq!(key_of("sw").as_deref(), Some("148%BlomPass"));
        assert!(result
            .warnings
            .iter()
            .any(|w| w.name == "sw" && w.reason.starts_with("key was revealed")));
    }

    /// From this version on the header names the AeroFTP version, which says
    /// for certain whether a field was written obscured. A header whose date
    /// does not parse says nothing, and the file is read as any rclone.conf.
    /// An old export holding an obscured empty key imports without one.
    #[test]
    fn test_import_rclone_reads_the_aeroftp_version_and_distrusts_a_bad_date() {
        let aws_obscured =
            "bgeHgvsu5PV96aZcpunIFC5EDvmaaHeNaaE7wuo2T7_GycW7y1zEI7a6f5iYPOi7Ropnc73qzcw";
        let import = |header: &str, secret: &str, tag: &str| {
            let conf = format!(
                "{header}[aws]\ntype = s3\nprovider = AWS\naccess_key_id = AKIA\nsecret_access_key = {secret}\n"
            );
            let path = tmp_write(
                &conf,
                &format!(
                    "aeroftp-test-import-version-{tag}-{}.conf",
                    std::process::id()
                ),
            );
            let result = import_rclone(&path).unwrap();
            std::fs::remove_file(&path).ok();
            result
        };
        let key = |r: &RcloneImportResult| r.servers[0].credential.clone();

        // Written by a version that keeps the key plain: as it is, no note,
        // whatever the date says.
        let current = import(
            "# Generated by AeroFTP 4.2.1 - https://aeroftp.app\n# Exported: 2026-04-20T10:00:00Z\n\n",
            aws_obscured,
            "current",
        );
        assert_eq!(key(&current).as_deref(), Some(aws_obscured));
        assert!(current.warnings.is_empty());

        // A pre-bump build writes 4.2.0 in the header. That line still names
        // the AeroFTP version, so the key is kept as written.
        let pre_bump = import(
            "# Generated by AeroFTP 4.2.0 - https://aeroftp.app\n# Exported: 2026-04-20T10:00:00Z\n\n",
            aws_obscured,
            "pre-bump",
        );
        assert_eq!(key(&pre_bump).as_deref(), Some(aws_obscured));
        assert!(pre_bump.warnings.is_empty());

        // Written by 3.7.4, whatever the date says: revealed.
        let old = import(
            "# Generated by AeroFTP 3.7.4 - https://aeroftp.app\n# Exported: 2026-09-01T10:00:00Z\n\n",
            aws_obscured,
            "old",
        );
        assert_eq!(
            key(&old).as_deref(),
            Some("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")
        );

        // No version and a date that does not parse: nothing to go by.
        let bad_date = import(
            "# Generated by AeroFTP - https://aeroftp.app\n# Exported: yesterday\n\n",
            aws_obscured,
            "bad-date",
        );
        assert_eq!(key(&bad_date).as_deref(), Some(aws_obscured));
        assert!(bad_date.warnings.is_empty());

        // `obscure("")` in a file that obscured the key: an empty key.
        let empty = import(
            "# Generated by AeroFTP - https://aeroftp.app\n# Exported: 2026-05-07T19:22:19Z\n\n",
            "NwRHnkk6Illbhv8J59q1OQ",
            "empty",
        );
        assert_eq!(key(&empty), None);
        assert!(
            empty.warnings[0].reason.contains("empty"),
            "{}",
            empty.warnings[0].reason
        );
    }

    /// A 22-character clear key under an old header reveals to empty, the same
    /// as `obscure("")`. It is dropped, and the note does not claim it was an
    /// obscured empty value.
    #[test]
    fn test_import_rclone_names_why_a_22_character_clear_key_is_dropped() {
        let clear = "A".repeat(22);
        assert!(
            reveal_rclone_password(&clear).unwrap().is_empty(),
            "a 22-character rclone-alphabet key reveals to empty, or this tests nothing"
        );
        let conf = format!(
            "# Generated by AeroFTP 3.7.4 - https://aeroftp.app\n\
             # Exported: 2026-09-01T10:00:00Z\n\n\
             [aws]\ntype = s3\nprovider = AWS\naccess_key_id = AKIA\n\
             secret_access_key = {clear}\n"
        );
        let path = tmp_write(
            &conf,
            &format!("aeroftp-test-import-22char-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(result.servers[0].credential.is_none());
        let reason = &result.warnings[0].reason;
        assert!(
            reason.contains("22 characters"),
            "the note must name the length, got: {reason}"
        );
        assert!(
            !reason.contains("obscured empty value"),
            "the note must not call a clear key an obscured empty value, got: {reason}"
        );
        assert!(!reason.contains(&clear));
    }

    /// A clear key short enough that its reveal is one printable byte used to
    /// be stored as that byte. It is kept as written, with a note.
    #[test]
    fn test_import_rclone_keeps_a_short_clear_key_that_reveals() {
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let clear = alphabet.iter().find_map(|&last| {
            let mut key = "A".repeat(22);
            key.push(last as char);
            let Ok(revealed) = reveal_rclone_password(&key) else {
                return None;
            };
            (revealed.len() == 1 && revealed.bytes().all(|b| (0x21..=0x7e).contains(&b)))
                .then_some(key)
        });
        let clear = clear.expect("a 23-character key that reveals to one printable byte");
        let conf = format!(
            "# Generated by AeroFTP 3.7.4 - https://aeroftp.app\n\
             # Exported: 2026-09-01T10:00:00Z\n\n\
             [aws]\ntype = s3\nprovider = AWS\naccess_key_id = AKIA\n\
             secret_access_key = {clear}\n"
        );
        let path = tmp_write(
            &conf,
            &format!("aeroftp-test-import-short-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            result.servers[0].credential.as_deref(),
            Some(clear.as_str())
        );
        let reason = &result.warnings[0].reason;
        assert!(
            reason.contains("kept as written"),
            "a too-short reveal must be noted, got: {reason}"
        );
        assert!(
            !reason.contains("was revealed:"),
            "the short key must not be accepted as revealed, got: {reason}"
        );
    }

    /// A real Swift password from before 4.0.5 that contains a space stays
    /// obscured, and the import says so instead of dropping the note.
    #[test]
    fn test_import_rclone_notes_a_swift_password_whose_reveal_has_a_space() {
        let obscured = obscure_password("pass word").expect("obscure");
        let revealed = reveal_rclone_password(&obscured).expect("reveal");
        assert!(
            revealed.contains(' '),
            "the fixture must reveal to a password with a space, got {revealed:?}"
        );
        let conf = format!(
            "# Generated by AeroFTP 4.0.4 - https://aeroftp.app\n\
             # Exported: 2026-09-01T10:00:00Z\n\n\
             [sw]\ntype = swift\nuser = demo\nkey = {obscured}\n\
             auth = https://auth.example.com/v3\n"
        );
        let path = tmp_write(
            &conf,
            &format!(
                "aeroftp-test-import-swift-space-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            result.servers[0].credential.as_deref(),
            Some(obscured.as_str())
        );
        let reason = &result.warnings[0].reason;
        assert!(
            reason.contains("kept as written") && reason.contains("space"),
            "got: {reason}"
        );
        assert!(!reason.contains("pass word"));
    }

    /// The export header names the version, which the import reads back.
    #[test]
    fn test_export_rclone_header_names_the_aeroftp_version() {
        let (_, conf) = export_to_string(&[export_server("f", "ftp", None)], "version-header");
        let header = format!(
            "# Generated by AeroFTP {} - https://aeroftp.app\n",
            env!("CARGO_PKG_VERSION")
        );
        assert!(conf.starts_with(&header), "{conf}");
        let export = aeroftp_export_header(&conf).expect("recognised as an AeroFTP export");
        assert_eq!(
            export.version,
            parse_aeroftp_version(env!("CARGO_PKG_VERSION"))
        );
        assert!(export.version.is_some() && export.exported.is_some());
    }

    /// A crypt remote whose password or salt rclone could not reveal is not
    /// imported. An unreadable salt used to be dropped, which selects rclone's
    /// default salt with the overlay enabled: a wrong key and no error. An
    /// omitted salt, or `obscure("")`, is rclone's default salt and imports.
    #[test]
    fn test_import_rclone_skips_a_crypt_remote_with_an_unreadable_secret() {
        let password = "I0foQLrVcrxA3fTR32MLs51K2uCFdW1sgw"; // rclone obscure topsecret
        let empty = "NwRHnkk6Illbhv8J59q1OQ"; // rclone obscure ""
        let conf = format!(
            "\
[base]
type = sftp
host = sftp.example.com
user = demo

[plain-salt]
type = crypt
remote = base:/a
password = {password}
password2 = saltsecret

[plain-password]
type = crypt
remote = base:/b
password = topsecret

[empty-password]
type = crypt
remote = base:/c
password = {empty}

[default-salt]
type = crypt
remote = base:/d
password = {password}
password2 = {empty}
"
        );
        let path = tmp_write(
            &conf,
            &format!(
                "aeroftp-test-import-crypt-unreadable-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();

        for (name, field) in [
            ("plain-salt", "password2"),
            ("plain-password", "password"),
            ("empty-password", "password"),
        ] {
            assert!(
                !result.servers.iter().any(|s| s.name == name),
                "{name} must not import"
            );
            let skipped = result
                .skipped
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} not reported"));
            assert!(
                skipped.reason.contains(&format!("{field} ")),
                "{name}: {}",
                skipped.reason
            );
            assert!(
                !skipped.reason.contains("saltsecret") && !skipped.reason.contains("topsecret")
            );
        }
        let default_salt = result
            .servers
            .iter()
            .find(|s| s.name == "default-salt")
            .expect("an obscured empty salt is rclone's default salt");
        let opts = default_salt.options.as_ref().unwrap();
        assert_eq!(opts["rcloneCryptPassword"], "topsecret");
        assert!(opts.get("rcloneCryptPassword2").is_none());
        assert!(result.servers.iter().any(|s| s.name == "base"));
    }

    /// rclone's `obscure.Reveal` decodes with Go's `RawURLEncoding`, which
    /// ignores the discarded low bits of the last symbol: rclone v1.75.1
    /// reveals the value below, the real `rclone obscure TestPass12` output
    /// with its final symbol changed in those bits only, as `TestPass12`. A
    /// value that reveals to nothing is reported rather than dropped.
    #[test]
    fn test_import_rclone_reveals_as_rclone_does_and_reports_an_empty_password() {
        let conf = "\
[trailing-bits]
type = ftp
host = ftp.example.com
user = demo
pass = qYdmOWprn6ieRZz_gEOedn17FVXSDmM7bHN

[empty]
type = ftp
host = ftp.example.com
user = demo
pass = NwRHnkk6Illbhv8J59q1OQ
";
        let path = tmp_write(
            conf,
            &format!(
                "aeroftp-test-import-reveal-edges-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let server = |name: &str| result.servers.iter().find(|s| s.name == name).unwrap();
        assert_eq!(
            server("trailing-bits").credential.as_deref(),
            Some("TestPass12")
        );
        assert_eq!(server("empty").credential, None);
        let note = result
            .warnings
            .iter()
            .find(|w| w.name == "empty")
            .expect("warned");
        assert!(
            note.reason.contains("pass reveals to an empty value"),
            "{}",
            note.reason
        );
        assert!(!result.warnings.iter().any(|w| w.name == "trailing-bits"));
    }

    /// A Cloudinary or ImageKit remote without the key AeroFTP signs with
    /// imports without a credential, and says so.
    #[test]
    fn test_import_rclone_reports_api_key_providers_without_their_secret() {
        let conf = "\
[cl]
type = cloudinary
cloud_name = demo-cloud
api_key = 1

[ik]
type = imagekit
endpoint = https://ik.imagekit.io/demo_id
";
        let path = tmp_write(
            conf,
            &format!(
                "aeroftp-test-import-api-key-no-secret-{}.conf",
                std::process::id()
            ),
        );
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        for (name, field) in [("cl", "api_secret"), ("ik", "private_key")] {
            assert!(
                result.servers.iter().any(|s| s.name == name),
                "{name} imports"
            );
            let note = result
                .warnings
                .iter()
                .find(|w| w.name == name)
                .unwrap_or_else(|| panic!("{name}: no warning"));
            assert!(note.reason.contains(field), "{name}: {}", note.reason);
        }
    }

    /// #128-D: `aeroftp_token_to_rclone` is the exact inverse of
    /// `rclone_token_to_aeroftp`. An rclone token blob converted into AeroFTP's
    /// `StoredTokens` shape and back must preserve every field, so an exported
    /// remote carries the same credentials rclone produced.
    #[test]
    fn test_token_conversion_roundtrips_both_ways() {
        let rclone_blob = r#"{
            "access_token": "ya29.aShortLived",
            "token_type": "Bearer",
            "refresh_token": "1//04longLivedRefresh",
            "expiry": "2030-06-01T08:30:00Z"
        }"#;
        let aero = rclone_token_to_aeroftp(rclone_blob).expect("rclone -> aero");
        let back = aeroftp_token_to_rclone(&aero).expect("aero -> rclone");
        let parsed: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(parsed["access_token"], "ya29.aShortLived");
        assert_eq!(parsed["token_type"], "Bearer");
        assert_eq!(parsed["refresh_token"], "1//04longLivedRefresh");
        // 2030-06-01T08:30:00Z survives the Unix round-trip.
        assert_eq!(parsed["expiry"], "2030-06-01T08:30:00Z");
    }

    /// A `StoredTokens` value with no usable access token must not produce a
    /// token line (the export arm then emits the reconnect guidance instead).
    #[test]
    fn test_aeroftp_token_to_rclone_rejects_empty_access() {
        assert!(aeroftp_token_to_rclone(r#"{"access_token":""}"#).is_none());
        assert!(aeroftp_token_to_rclone(r#"{"refresh_token":"r"}"#).is_none());
    }

    /// #128-D: every OAuth-token provider exports a USABLE rclone remote when
    /// the export path injected the token + BYO client_id/secret, and the token
    /// re-imports to the same AeroFTP profile. Drive/Dropbox/OneDrive/Box/
    /// pCloud/Yandex share the same code path; the table pins each backend type.
    #[test]
    fn test_export_rclone_oauth_providers_roundtrip() {
        let cases = [
            ("googledrive", "drive"),
            ("dropbox", "dropbox"),
            ("onedrive", "onedrive"),
            ("box", "box"),
            ("pcloud", "pcloud"),
            ("yandexdisk", "yandex"),
            ("zohoworkdrive", "zoho"),
        ];
        // The injected token is AeroFTP's StoredTokens shape (Unix expires_at).
        let stored_tokens = r#"{"access_token":"acc-123","refresh_token":"ref-456","expires_at":1893499200,"token_type":"Bearer","scopes":[]}"#;
        for (protocol, rclone_type) in cases {
            let servers = vec![RcloneExportServer {
                id: format!("{}-acct", protocol),
                name: format!("{}-acct", protocol),
                host: "example.com".to_string(),
                port: 443,
                username: "me@example.com".to_string(),
                protocol: Some(protocol.to_string()),
                options: Some(serde_json::json!({
                    "__aeroftp_oauth_token": stored_tokens,
                    "__aeroftp_oauth_client_id": "client-id-xyz",
                    "__aeroftp_oauth_client_secret": "client-secret-xyz",
                    // Only zoho reads these two; the others ignore them. Zoho
                    // is skipped without a root_folder_id, and this table is
                    // about the backend type each protocol writes.
                    "region": "us",
                    "root_folder_id": "space-1",
                })),
                provider_id: Some(protocol.to_string()),
            }];
            let passwords = HashMap::new();
            let tmp = std::env::temp_dir().join(format!("aeroftp-test-oauth-{}.conf", protocol));
            export_rclone(&servers, &passwords, &tmp).expect("should export");
            let conf = std::fs::read_to_string(&tmp).expect("read conf");
            std::fs::remove_file(&tmp).ok();

            assert!(
                conf.contains(&format!("type = {}", rclone_type)),
                "{protocol}: expected rclone type {rclone_type}:\n{conf}"
            );
            assert!(
                conf.contains("client_id = client-id-xyz"),
                "{protocol}: client_id must be emitted:\n{conf}"
            );
            assert!(
                conf.contains("client_secret = client-secret-xyz"),
                "{protocol}: client_secret must be emitted:\n{conf}"
            );
            assert!(
                conf.contains("token = ") && conf.contains("acc-123"),
                "{protocol}: token blob must be emitted:\n{conf}"
            );
            // The private injection keys must never leak into the config.
            assert!(
                !conf.contains("__aeroftp_oauth"),
                "{protocol}: private injection keys must not leak:\n{conf}"
            );

            // Re-import recovers the token onto the same protocol.
            let result = import_rclone(&tmp_write(
                &conf,
                &format!("aeroftp-reimport-{}.conf", protocol),
            ))
            .unwrap();
            let server = result
                .servers
                .iter()
                .find(|s| s.protocol.as_deref() == Some(protocol))
                .unwrap_or_else(|| panic!("{protocol}: server must re-import:\n{conf}"));
            let secrets = result
                .provider_secrets
                .get(&server.id)
                .unwrap_or_else(|| panic!("{protocol}: provider_secrets must carry the token"));
            let oauth = secrets.oauth.as_deref().expect("oauth blob present");
            let parsed: serde_json::Value = serde_json::from_str(oauth).unwrap();
            assert_eq!(parsed["access_token"], "acc-123", "{protocol}");
            assert_eq!(parsed["refresh_token"], "ref-456", "{protocol}");
            // BYO app credentials recovered for a fresh-device reconnect.
            assert_eq!(
                secrets.oauth_client_id.as_deref(),
                Some("client-id-xyz"),
                "{protocol}: client_id must round-trip"
            );
            assert_eq!(
                secrets.oauth_client_secret.as_deref(),
                Some("client-secret-xyz"),
                "{protocol}: client_secret must round-trip"
            );
        }
    }

    #[test]
    fn test_zoho_region_maps_between_aeroftp_and_rclone() {
        assert_eq!(zoho_region_to_rclone("us").as_deref(), Some("com"));
        assert_eq!(zoho_region_to_rclone("au").as_deref(), Some("com.au"));
        assert_eq!(zoho_region_to_rclone("cn").as_deref(), Some("com.cn"));
        assert_eq!(zoho_region_to_rclone("eu").as_deref(), Some("eu"));
        assert_eq!(zoho_region_to_rclone("uk").as_deref(), Some("uk"));
        assert_eq!(zoho_region_to_rclone("sa").as_deref(), Some("sa"));
        assert_eq!(zoho_region_to_rclone("ae").as_deref(), Some("ae"));
        assert_eq!(zoho_region_to_rclone("ca"), None);
        assert_eq!(zoho_region_to_rclone("not-a-dc"), None);
        assert_eq!(zoho_region_from_rclone("com"), "us");
        assert_eq!(zoho_region_from_rclone("com.au"), "au");
        assert_eq!(zoho_region_from_rclone("eu"), "eu");
    }

    #[test]
    fn test_export_rclone_zoho_emits_region_and_root_folder() {
        let stored_tokens = r#"{"access_token":"acc-zoho","refresh_token":"ref-zoho","expires_at":1893499200,"token_type":"Zoho-oauthtoken","scopes":[]}"#;
        let servers = vec![RcloneExportServer {
            id: "zoho-eu".to_string(),
            name: "zoho-eu".to_string(),
            host: "workdrive.zoho.eu".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("zohoworkdrive".to_string()),
            options: Some(serde_json::json!({
                "region": "eu",
                "root_folder_id": "4u28602177065ff22426787a6745dba8954eb",
                "__aeroftp_oauth_token": stored_tokens,
                "__aeroftp_oauth_client_id": "cid",
                "__aeroftp_oauth_client_secret": "csec",
            })),
            provider_id: Some("zoho-workdrive".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-export-zoho.conf");
        export_rclone(&servers, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(conf.contains("type = zoho"), "type:\n{conf}");
        assert!(conf.contains("region = eu"), "region:\n{conf}");
        assert!(
            conf.contains("root_folder_id = 4u28602177065ff22426787a6745dba8954eb"),
            "root_folder_id:\n{conf}"
        );
        assert!(
            conf.contains("token = ") && conf.contains("acc-zoho"),
            "token:\n{conf}"
        );
        assert!(
            conf.contains("Zoho-oauthtoken"),
            "zoho token_type must be Zoho-oauthtoken:\n{conf}"
        );
        assert!(
            !conf.contains("__aeroftp_oauth"),
            "private keys leaked:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_zoho_rewrites_bearer_token_type() {
        let stored_tokens = r#"{"access_token":"acc-zoho","refresh_token":"ref-zoho","expires_at":1893499200,"token_type":"Bearer","scopes":[]}"#;
        let servers = vec![RcloneExportServer {
            id: "zoho-bearer".to_string(),
            name: "zoho-bearer".to_string(),
            host: "workdrive.zoho.eu".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("zohoworkdrive".to_string()),
            options: Some(serde_json::json!({
                "region": "eu",
                // rclone cannot list a zoho remote without the privatespace id,
                // so the writer refuses a profile that has none: this fixture
                // is about the token type, and needs a profile that gets written.
                "root_folder_id": "space-1",
                "__aeroftp_oauth_token": stored_tokens,
                "__aeroftp_oauth_client_id": "cid",
                "__aeroftp_oauth_client_secret": "csec",
            })),
            provider_id: Some("zoho-workdrive".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-export-zoho-bearer.conf");
        export_rclone(&servers, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(
            conf.contains("Zoho-oauthtoken"),
            "Bearer must be rewritten for rclone:\n{conf}"
        );
        assert!(
            !conf.contains("\"token_type\":\"Bearer\""),
            "must not leave Bearer in the rclone token:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_zoho_us_region_becomes_com() {
        let servers = vec![RcloneExportServer {
            id: "zoho-us".to_string(),
            name: "zoho-us".to_string(),
            host: "workdrive.zoho.com".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("zohoworkdrive".to_string()),
            // Same reason as above: without a root_folder_id the profile is
            // skipped, and this test is about the region slug.
            options: Some(serde_json::json!({ "region": "us", "root_folder_id": "space-1" })),
            provider_id: Some("zoho-workdrive".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-export-zoho-us.conf");
        export_rclone(&servers, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(
            conf.contains("region = com"),
            "us must map to rclone com:\n{conf}"
        );
        assert!(
            !conf.contains("region = us"),
            "must not emit AeroFTP slug:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_zoho_skips_region_rclone_cannot_address() {
        let servers = vec![RcloneExportServer {
            id: "zoho-ca".to_string(),
            name: "zoho-ca".to_string(),
            host: "workdrive.zohocloud.ca".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("zohoworkdrive".to_string()),
            options: Some(serde_json::json!({ "region": "ca" })),
            provider_id: Some("zoho-workdrive".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-export-zoho-ca.conf");
        let n = export_rclone(&servers, &HashMap::new(), &tmp)
            .expect("export")
            .exported;
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert_eq!(
            n, 0,
            "unusable zoho remote must not count as exported:\n{conf}"
        );
        assert!(
            !conf.contains("[zoho-ca]"),
            "must not emit a section rclone cannot reach:\n{conf}"
        );
        assert!(
            conf.contains("skipped Zoho profile 'zoho-ca'"),
            "skip must be explained:\n{conf}"
        );
    }

    #[test]
    fn test_import_rclone_zoho_keeps_root_folder_off_the_path() {
        let conf = "\
[MyZohoWD]
type = zoho
region = eu
root_folder_id = fmip966f979e195e64ec78e6846976861eed5
token = {\"access_token\":\"acc\",\"token_type\":\"Zoho-oauthtoken\",\"refresh_token\":\"ref\",\"expiry\":\"2030-01-01T00:00:00Z\"}
";
        let path = tmp_write(conf, "aeroftp-test-import-zoho.conf");
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let zoho = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("zohoworkdrive"))
            .expect("zoho server present");
        assert!(
            zoho.initial_path.is_none(),
            "root_folder_id must not become initial_path: {:?}",
            zoho.initial_path
        );
        assert_eq!(
            zoho.options
                .as_ref()
                .and_then(|o| o.get("root_folder_id"))
                .and_then(|v| v.as_str()),
            Some("fmip966f979e195e64ec78e6846976861eed5")
        );
        assert_eq!(
            zoho.options
                .as_ref()
                .and_then(|o| o.get("region"))
                .and_then(|v| v.as_str()),
            Some("eu")
        );
    }

    /// The key is derived from a crypt password as it is, spaces included, so
    /// the export must not trim it: rclone would derive another key.
    #[test]
    fn test_export_rclone_crypt_password_is_not_trimmed() {
        let servers = vec![export_server(
            "vault",
            "sftp",
            Some(serde_json::json!({
                "rcloneCryptEnabled": true,
                "rcloneCryptPassword": " pass with spaces ",
                "rcloneCryptPassword2": " salt ",
            })),
        )];
        let (_, conf) = export_to_string(&servers, "crypt-no-trim");
        let value = |key: &str| {
            conf.lines()
                .find_map(|l| l.strip_prefix(&format!("{key} = ")))
                .unwrap_or_else(|| panic!("no {key}:\n{conf}"))
                .to_string()
        };
        assert_eq!(
            reveal_obscured(&value("password")).as_deref(),
            Ok(" pass with spaces ")
        );
        assert_eq!(
            reveal_obscured(&value("password2")).as_deref(),
            Ok(" salt ")
        );

        // Spaces only are a secret too: left out, a salt of spaces would
        // select rclone's default salt.
        let servers = vec![export_server(
            "vault",
            "sftp",
            Some(serde_json::json!({
                "rcloneCryptEnabled": true,
                "rcloneCryptPassword": "   ",
                "rcloneCryptPassword2": "  ",
            })),
        )];
        let (_, conf) = export_to_string(&servers, "crypt-spaces-only");
        let value = |key: &str| {
            conf.lines()
                .find_map(|l| l.strip_prefix(&format!("{key} = ")))
                .unwrap_or_else(|| panic!("no {key}:\n{conf}"))
                .to_string()
        };
        assert_eq!(reveal_obscured(&value("password")).as_deref(), Ok("   "));
        assert_eq!(reveal_obscured(&value("password2")).as_deref(), Ok("  "));
    }

    #[test]
    fn test_export_rclone_crypt_emits_second_section() {
        let servers = vec![RcloneExportServer {
            id: "My NAS".to_string(),
            name: "My NAS".to_string(),
            host: "192.168.1.10".to_string(),
            port: 22,
            username: "admin".to_string(),
            protocol: Some("sftp".to_string()),
            options: Some(serde_json::json!({
                "rcloneCryptEnabled": true,
                "rcloneCryptRemote": "mynas:/encrypted",
                "rcloneCryptOverlayName": "vault",
                "rcloneCryptPassword": "topsecret",
                "rcloneCryptPassword2": "saltsecret",
                "rcloneCryptFilenameEncryption": "standard",
                "rcloneCryptDirectoryNameEncryption": true,
            })),
            provider_id: None,
        }];
        let mut passwords = HashMap::new();
        passwords.insert("My NAS".to_string(), "sftppass".to_string());
        let tmp = std::env::temp_dir().join("aeroftp-test-export-crypt.conf");
        let n = export_rclone(&servers, &passwords, &tmp)
            .expect("export")
            .exported;
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert_eq!(n, 2, "base + crypt remotes:\n{conf}");
        assert!(conf.contains("[My NAS]"), "base section:\n{conf}");
        assert!(conf.contains("type = sftp"), "base type:\n{conf}");
        assert!(conf.contains("[vault]"), "crypt section name:\n{conf}");
        assert!(conf.contains("type = crypt"), "crypt type:\n{conf}");
        assert!(
            conf.contains("remote = My NAS:/encrypted"),
            "crypt remote rewritten to this export's base name:\n{conf}"
        );
        assert!(
            conf.contains("password = ") && !conf.contains("password = topsecret"),
            "crypt password must be obscured:\n{conf}"
        );
        assert!(
            conf.contains("password2 = ") && !conf.contains("password2 = saltsecret"),
            "crypt salt must be obscured:\n{conf}"
        );
        assert!(
            conf.contains("filename_encryption = standard"),
            "filename mode:\n{conf}"
        );
        assert!(
            conf.contains("directory_name_encryption = true"),
            "dir mode:\n{conf}"
        );
    }

    /// A crypt remote `vault` whose base is `base:/enc` imports as one sftp
    /// profile named vault, starting in /enc. Export must not claim the
    /// section `vault` for the crypt remote: that name is the clear profile,
    /// and `rclone sync vault:` has to keep pointing at it. The crypt section
    /// wraps the start-folder alias, the same fallback the root case uses.
    #[test]
    fn test_export_rclone_crypt_in_a_subfolder_does_not_take_the_profile_name() {
        let password = obscure_password("topsecret").expect("obscure");
        let conf = format!(
            "[base]\ntype = sftp\nhost = sftp.example.com\nuser = demo\npass = {password}\n\n\
             [vault]\ntype = crypt\nremote = base:/enc\npassword = {password}\n"
        );
        let path = tmp_write(
            &conf,
            &format!("aeroftp-test-crypt-subfolder-{}.conf", std::process::id()),
        );
        let imported = import_rclone(&path).expect("import");
        std::fs::remove_file(&path).ok();
        let vault = imported
            .servers
            .iter()
            .find(|s| s.name == "vault")
            .expect("profile vault");
        assert_eq!(vault.initial_path.as_deref(), Some("/enc"));
        let mut options = vault.options.clone().unwrap_or_default();
        options
            .as_object_mut()
            .unwrap()
            .insert("initial_path".into(), serde_json::json!("/enc"));
        let servers = vec![RcloneExportServer {
            id: vault.id.clone(),
            name: vault.name.clone(),
            host: vault.host.clone(),
            port: vault.port,
            username: vault.username.clone(),
            protocol: vault.protocol.clone(),
            options: Some(options),
            provider_id: vault.provider_id.clone(),
        }];
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-test-export-crypt-subfolder-{}.conf",
            std::process::id()
        ));
        let outcome = export_rclone(&servers, &HashMap::new(), &tmp).expect("export");
        let exported = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(
            outcome.skipped.is_empty(),
            "the crypt section must be written, not refused: {:?}\n{exported}",
            outcome.skipped
        );
        assert!(
            exported.contains("type = crypt"),
            "crypt section:\n{exported}"
        );
        assert!(
            exported.contains("[vault-path]"),
            "start-folder alias:\n{exported}"
        );
        assert!(
            exported.contains("remote = vault-path:\n"),
            "crypt must wrap the alias at its root, not at vault-path:/enc, which is \
             vault:/enc/enc:\n{exported}"
        );
        let crypt_header = exported
            .lines()
            .find(|line| line.starts_with('[') && line.contains("crypt"))
            .unwrap_or("");
        assert_ne!(
            crypt_header, "[vault]",
            "crypt must not take the clear name"
        );
        // L8 of the 4.2.1 closeout: the note is a warning, with the real
        // names, that the name the operator knew as the crypt is the clear
        // remote in this file.
        let crypt_name = crypt_header.trim_matches(['[', ']']);
        assert!(
            exported.contains("`vault:` in this file is NOT encrypted"),
            "the note must warn that the clear name is not encrypted:\n{exported}"
        );
        assert!(
            exported.contains(&format!("use `{crypt_name}:`")),
            "the note must name the crypt section to use instead:\n{exported}"
        );

        // The round trip: importing this export gives back the crypt profile
        // on the same server, starting in the same folder, with the same key.
        let reimport = std::env::temp_dir().join(format!(
            "aeroftp-test-reimport-crypt-subfolder-{}.conf",
            std::process::id()
        ));
        std::fs::write(&reimport, &exported).expect("write");
        let again = import_rclone(&reimport).expect("re-import");
        std::fs::remove_file(&reimport).ok();
        let crypt = again
            .servers
            .iter()
            .find(|s| {
                s.options
                    .as_ref()
                    .and_then(|o| o.get("rcloneCryptEnabled"))
                    .and_then(|v| v.as_bool())
                    == Some(true)
            })
            .unwrap_or_else(|| {
                panic!(
                    "the crypt remote must come back as a crypt profile, skipped: {:?}\n{exported}",
                    again
                        .skipped
                        .iter()
                        .map(|s| format!("{}: {}", s.name, s.reason))
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(crypt.name, crypt_name);
        assert_eq!(crypt.host, "sftp.example.com");
        assert_eq!(crypt.username, "demo");
        assert_eq!(
            crypt.initial_path.as_deref(),
            Some("/enc"),
            "the crypt must open in the folder it was exported from:\n{exported}"
        );
        assert_eq!(
            crypt
                .options
                .as_ref()
                .and_then(|o| o.get("rcloneCryptPassword"))
                .and_then(|v| v.as_str()),
            Some("topsecret")
        );
    }

    /// The imported crypt path is from the clear remote's root, the alias
    /// base already points into it: the path is taken relative to the
    /// alias's folder, and one outside it wraps the clear remote.
    #[test]
    fn crypt_export_remote_target_is_relative_to_the_alias_folder() {
        let imported = |target: &str| serde_json::json!({ "rcloneCryptRemote": target });
        let at = |target: &str| {
            crypt_export_remote_target(
                "nas-path",
                "nas",
                Some(AliasRoot::Folder("/team")),
                None,
                &imported(target),
            )
        };
        assert_eq!(at("x:/team"), "nas-path:");
        assert_eq!(at("x:team/vault"), "nas-path:/vault");
        assert_eq!(at("x:/other"), "nas:/other");
        assert_eq!(
            at("x:/teamwork"),
            "nas:/teamwork",
            "a prefix is not a folder"
        );
        assert_eq!(
            crypt_export_remote_target("nas", "nas", None, None, &imported("x:/enc")),
            "nas:/enc"
        );
        assert_eq!(
            crypt_export_remote_target(
                "nas-path",
                "nas",
                Some(AliasRoot::Folder("/team")),
                None,
                &serde_json::json!({ "rcloneCryptOverlayScope": "/vault" })
            ),
            "nas-path:/vault",
            "the overlay scope is already relative to the start folder"
        );
        // No alias points at the folder: the crypt wraps the clear remote
        // there, as the alias would have held it. An imported path is from
        // the clear remote's root and does not need it.
        let scoped = |scope: &str| serde_json::json!({ "rcloneCryptOverlayScope": scope });
        let unaliased = |folder: &str, options: &serde_json::Value| {
            crypt_export_remote_target("nas", "nas", None, Some(folder), options)
        };
        assert_eq!(unaliased("/data ", &scoped("/vault")), "nas:/data /vault");
        assert_eq!(unaliased("data/", &scoped("vault/")), "nas:data/vault");
        assert_eq!(unaliased("/data ", &scoped("")), "nas:/data ");
        assert_eq!(unaliased("/data ", &imported("x:/enc")), "nas:/enc");
    }

    /// A crypt over an alias imports onto what the alias points at; a chain
    /// is followed, and one that loops is refused instead of spinning.
    #[test]
    fn a_crypt_over_an_alias_resolves_to_the_aliased_remote() {
        let section = |pairs: &[(&str, &str)]| -> RcloneRemote {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let sections: HashMap<String, RcloneRemote> = HashMap::from([
            ("nas".to_string(), section(&[("type", "sftp")])),
            (
                "nas-path".to_string(),
                section(&[("type", "alias"), ("remote", "nas:/team")]),
            ),
            (
                "deeper".to_string(),
                section(&[("type", "alias"), ("remote", "nas-path:docs")]),
            ),
            (
                "loop".to_string(),
                section(&[("type", "alias"), ("remote", "loop:x")]),
            ),
        ]);
        assert_eq!(
            resolve_alias_target("nas-path:", &sections).unwrap(),
            "nas:/team"
        );
        assert_eq!(
            resolve_alias_target("nas-path:/vault", &sections).unwrap(),
            "nas:/team/vault"
        );
        assert_eq!(
            resolve_alias_target("deeper:v", &sections).unwrap(),
            "nas:/team/docs/v"
        );
        assert_eq!(
            resolve_alias_target("nas:/enc", &sections).unwrap(),
            "nas:/enc"
        );
        assert!(resolve_alias_target("loop:", &sections).is_err());
    }

    #[test]
    fn test_export_rclone_crypt_scope_and_missing_password() {
        let servers = vec![RcloneExportServer {
            id: "drive".to_string(),
            name: "drive".to_string(),
            host: "www.googleapis.com".to_string(),
            port: 443,
            username: "me".to_string(),
            protocol: Some("googledrive".to_string()),
            options: Some(serde_json::json!({
                "rcloneCryptEnabled": true,
                "rcloneCryptOverlayScope": "/vault",
            })),
            provider_id: Some("googledrive".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-export-crypt-scope.conf");
        export_rclone(&servers, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(
            conf.contains("[drive-crypt]"),
            "default crypt name:\n{conf}"
        );
        assert!(
            conf.contains("remote = drive:/vault"),
            "scope becomes remote path:\n{conf}"
        );
        assert!(
            conf.contains("# password required but unavailable"),
            "missing password must not emit a broken line:\n{conf}"
        );
        let has_pw_line = conf
            .lines()
            .any(|l| l.trim_start().starts_with("password ="));
        assert!(!has_pw_line, "must not emit password =:\n{conf}");
    }

    #[test]
    fn test_export_rclone_s3_crypt_wraps_bucket_alias() {
        let servers = vec![RcloneExportServer {
            id: "minio".to_string(),
            name: "minio".to_string(),
            host: "s3.lab.example.test".to_string(),
            port: 443,
            username: "AKIAEXAMPLE".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({
                "region": "us-east-1",
                "endpoint": "https://s3.lab.example.test",
                "bucket": "aeroftp-test",
                "rcloneCryptEnabled": true,
                "rcloneCryptOverlayScope": "/vault",
                "rcloneCryptPassword": "topsecret",
            })),
            provider_id: Some("minio".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("minio".to_string(), "s3secret".to_string());
        let tmp = std::env::temp_dir().join("aeroftp-test-export-s3-crypt-alias.conf");
        let n = export_rclone(&servers, &passwords, &tmp)
            .expect("export")
            .exported;
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(n >= 2, "s3 + alias + crypt: {n}\n{conf}");
        assert!(
            conf.contains("[minio-aeroftp-test]"),
            "bucket alias:\n{conf}"
        );
        assert!(
            conf.contains("remote = minio-aeroftp-test:/vault"),
            "crypt must wrap the bucket alias, not minio:/vault:\n{conf}"
        );
        assert!(
            !conf.contains("remote = minio:/vault"),
            "must not point crypt at a bucket named vault:\n{conf}"
        );
    }

    /// #128-D: pCloud's rclone `hostname` must be a real API host, not the
    /// display label AeroFTP stores in `host`. US omits it (rclone default
    /// api.pcloud.com); EU emits eapi.pcloud.com. Region may arrive via the
    /// host label or `options.region`.
    #[test]
    fn test_export_rclone_pcloud_region_hostname() {
        // US profile: host is a display label, no options.region -> no hostname.
        let us = vec![RcloneExportServer {
            id: "pcloud-us".to_string(),
            name: "pcloud-us".to_string(),
            host: "pCloud (US)".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("pcloud".to_string()),
            options: None,
            provider_id: Some("pcloud".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-pcloud-us.conf");
        export_rclone(&us, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(conf.contains("type = pcloud"), "type:\n{conf}");
        assert!(
            !conf.contains("hostname ="),
            "US must omit hostname (rclone defaults to api.pcloud.com):\n{conf}"
        );
        assert!(
            !conf.contains("pCloud (US)"),
            "the display label must never be emitted as a hostname:\n{conf}"
        );

        // EU via the host label.
        let eu = vec![RcloneExportServer {
            id: "pcloud-eu".to_string(),
            name: "pcloud-eu".to_string(),
            host: "pCloud (EU)".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("pcloud".to_string()),
            options: None,
            provider_id: Some("pcloud".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-pcloud-eu.conf");
        export_rclone(&eu, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(
            conf.contains("hostname = eapi.pcloud.com"),
            "EU must emit eapi.pcloud.com:\n{conf}"
        );

        // EU via options.region, and the import maps the hostname back to region.
        let eu2 = vec![RcloneExportServer {
            id: "pcloud-eu2".to_string(),
            name: "pcloud-eu2".to_string(),
            host: "anything".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("pcloud".to_string()),
            options: Some(serde_json::json!({ "region": "EU" })),
            provider_id: Some("pcloud".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-pcloud-eu2.conf");
        export_rclone(&eu2, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(conf.contains("hostname = eapi.pcloud.com"), "EU2:\n{conf}");
        let result = import_rclone(&tmp_write(&conf, "aeroftp-reimport-pcloud-eu.conf")).unwrap();
        let p = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("pcloud"))
            .expect("pcloud server");
        assert_eq!(p.host, "eapi.pcloud.com", "host maps to EU API");
        assert_eq!(
            p.options
                .as_ref()
                .and_then(|o| o.get("region"))
                .and_then(|v| v.as_str()),
            Some("eu"),
            "imported region must be EU"
        );
    }

    /// Without injected secrets the OAuth arm must emit a `reconnect` guidance
    /// comment, never a half-formed `token =` line: no silently broken remote.
    #[test]
    fn test_export_rclone_oauth_without_secrets_emits_reconnect_comment() {
        let servers = vec![RcloneExportServer {
            id: "drive-noauth".to_string(),
            name: "drive-noauth".to_string(),
            host: "www.googleapis.com".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("googledrive".to_string()),
            options: None,
            provider_id: Some("googledrive".to_string()),
        }];
        let passwords = HashMap::new();
        let tmp = std::env::temp_dir().join("aeroftp-test-oauth-noauth.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(conf.contains("type = drive"), "type present:\n{conf}");
        assert!(
            !conf.contains("token = "),
            "must not emit a token line without credentials:\n{conf}"
        );
        assert!(
            conf.contains("rclone config reconnect"),
            "must emit reconnect guidance:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_webdav_nextcloud_appends_dav_path() {
        // Bug (2026-06-03): exporting a Nextcloud profile with a bare host
        // wrote `url = https://host` without the `/remote.php/dav/files/<user>/`
        // collection root rclone's nextcloud vendor requires, so the exported
        // remote could not list or transfer.
        let servers = vec![RcloneExportServer {
            id: "ncloud".to_string(),
            name: "ncloud".to_string(),
            host: "https://cloud.lab.example.test".to_string(),
            port: 443,
            username: "alice".to_string(),
            protocol: Some("webdav".to_string()),
            options: None,
            provider_id: Some("nextcloud".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("ncloud".to_string(), "ncsecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-webdav-nc.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("vendor = nextcloud"),
            "expected nextcloud vendor:\n{conf}"
        );
        assert!(
            conf.contains("url = https://cloud.lab.example.test/remote.php/dav/files/alice/"),
            "must synthesise the DAV collection root:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_webdav_owncloud_appends_dav_path() {
        let servers = vec![RcloneExportServer {
            id: "ocloud".to_string(),
            name: "ocloud".to_string(),
            host: "https://oc.example.com".to_string(),
            port: 443,
            username: "bob".to_string(),
            protocol: Some("webdav".to_string()),
            options: None,
            provider_id: Some("owncloud".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("ocloud".to_string(), "ocsecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-webdav-oc.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("vendor = owncloud"),
            "expected owncloud vendor:\n{conf}"
        );
        assert!(
            conf.contains("url = https://oc.example.com/remote.php/dav/files/bob/"),
            "must synthesise the DAV collection root:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_webdav_nextcloud_existing_dav_path_not_duplicated() {
        // A profile that already carries a `/remote.php/...` path (basePath or
        // explicit URL) must be honoured verbatim, never double-appended.
        let servers = vec![RcloneExportServer {
            id: "ncloud".to_string(),
            name: "ncloud".to_string(),
            host: "https://cloud.lab.example.test".to_string(),
            port: 443,
            username: "alice".to_string(),
            protocol: Some("webdav".to_string()),
            options: Some(serde_json::json!({
                "basePath": "/remote.php/dav/files/alice/"
            })),
            provider_id: Some("nextcloud".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("ncloud".to_string(), "ncsecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-webdav-nc-dup.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("url = https://cloud.lab.example.test/remote.php/dav/files/alice/"),
            "existing DAV path must be kept as-is:\n{conf}"
        );
        assert!(
            !conf.contains("dav/files/alice/remote.php"),
            "must not double-append the DAV path:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_webdav_generic_vendor_untouched() {
        // Generic WebDAV (vendor `other`: Koofr, SharePoint, Fastmail) has no
        // Nextcloud collection convention and must be left exactly as-is.
        let servers = vec![RcloneExportServer {
            id: "generic".to_string(),
            name: "generic".to_string(),
            host: "https://dav.example.com".to_string(),
            port: 443,
            username: "carol".to_string(),
            protocol: Some("webdav".to_string()),
            options: None,
            provider_id: Some("webdav".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("generic".to_string(), "gsecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-webdav-generic.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("vendor = other"),
            "expected generic vendor=other:\n{conf}"
        );
        assert!(
            conf.contains("url = https://dav.example.com\n"),
            "generic WebDAV url must be untouched:\n{conf}"
        );
        assert!(
            !conf.contains("/remote.php/"),
            "must not inject a Nextcloud DAV path on generic WebDAV:\n{conf}"
        );
    }

    #[test]
    fn test_export_backblaze_s3_uses_supported_rclone_provider() {
        let dir = tempfile::tempdir().unwrap();
        for provider_id in ["backblaze", "backblaze-b2"] {
            let servers = vec![RcloneExportServer {
                id: "b2-s3".into(),
                name: "b2-s3".into(),
                host: "s3.eu-central-003.backblazeb2.com".into(),
                port: 443,
                username: "test-key".into(),
                protocol: Some("s3".into()),
                options: Some(serde_json::json!({"bucket": "test-bucket"})),
                provider_id: Some(provider_id.into()),
            }];
            let path = dir.path().join("rclone.conf");
            export_rclone(&servers, &HashMap::new(), &path).unwrap();
            let config = std::fs::read_to_string(path).unwrap();
            assert!(config.contains("provider = Other\n"));
            assert!(!config.contains("provider = Backblaze"));
            assert!(config.contains("directory_markers = true\n"));
            assert!(config.contains("s3.eu-central-003.backblazeb2.com"));
        }
    }

    #[test]
    fn test_export_tigris_s3_is_other_with_explicit_region_and_addressing() {
        let export = |options: serde_json::Value, host: &str| {
            let servers = vec![RcloneExportServer {
                id: "tigris".into(),
                name: "tigris".into(),
                host: host.into(),
                port: 443,
                username: "tid_test".into(),
                protocol: Some("s3".into()),
                options: Some(options),
                provider_id: Some("tigris".into()),
            }];
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("rclone.conf");
            export_rclone(&servers, &HashMap::new(), &path).unwrap();
            std::fs::read_to_string(path).unwrap()
        };

        // Preset defaults only: the endpoint comes from the manifest, region
        // and addressing are written explicitly.
        let conf = export(serde_json::json!({ "bucket": "photos" }), "");
        assert!(conf.contains("provider = Other\n"), "{conf}");
        assert!(
            conf.contains("endpoint = https://t3.storage.dev\n"),
            "{conf}"
        );
        assert!(conf.contains("region = auto\n"), "{conf}");
        assert!(conf.contains("force_path_style = false\n"), "{conf}");
        assert_eq!(conf.matches("region = ").count(), 1, "{conf}");

        // A stored region is not duplicated and a stored addressing choice wins.
        let conf = export(
            serde_json::json!({ "bucket": "photos", "region": "auto", "pathStyle": true }),
            "fly.storage.tigris.dev",
        );
        assert_eq!(conf.matches("region = ").count(), 1, "{conf}");
        assert!(
            conf.contains("endpoint = https://fly.storage.tigris.dev\n"),
            "{conf}"
        );
        assert!(conf.contains("force_path_style = true\n"), "{conf}");

        // A pinned bucket with dots defaults to path-style; an explicit
        // choice still wins.
        let conf = export(serde_json::json!({ "bucket": "photos.example" }), "");
        assert!(conf.contains("force_path_style = true\n"), "{conf}");
        let conf = export(
            serde_json::json!({ "bucket": "photos.example", "pathStyle": false }),
            "",
        );
        assert!(conf.contains("force_path_style = false\n"), "{conf}");

        // A blank region is no region: `auto` is written once.
        for blank in ["", "   "] {
            let conf = export(
                serde_json::json!({ "bucket": "photos", "region": blank }),
                "",
            );
            assert!(conf.contains("region = auto\n"), "{conf}");
            assert_eq!(conf.matches("region = ").count(), 1, "{conf}");
        }

        // A string addressing choice counts like a bool, in either case.
        for (stored, expected) in [("true", true), ("TRUE", true), (" false ", false)] {
            let conf = export(
                serde_json::json!({ "bucket": "photos.example", "path_style": stored }),
                "",
            );
            assert!(
                conf.contains(&format!("force_path_style = {expected}\n")),
                "{stored}: {conf}"
            );
        }
        let conf = export(
            serde_json::json!({ "bucket": "photos", "pathStyle": "true" }),
            "",
        );
        assert!(conf.contains("force_path_style = true\n"), "{conf}");
    }

    #[test]
    fn test_export_rclone_s3_registry_endpoint_fallback() {
        // #3 regression: Google Cloud Storage S3-interop profiles store the
        // bucket but not the endpoint (it comes from the provider registry).
        // Before the fix the export emitted no endpoint and rclone fell back
        // to the unreachable AWS auto-region host. The export must now resolve
        // the preset endpoint just like the connect path.
        let servers = vec![RcloneExportServer {
            id: "gcs".to_string(),
            name: "gcs".to_string(),
            host: String::new(),
            port: 443,
            username: "GOOGTESTKEY".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({ "bucket": "gcsbucket" })),
            provider_id: Some("google-cloud-storage".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("gcs".to_string(), "gcssecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-s3-gcs.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("endpoint = https://storage.googleapis.com"),
            "must resolve the GCS preset endpoint:\n{conf}"
        );
        assert!(
            !conf.contains("amazonaws.com"),
            "must not leave rclone to fall back to the AWS auto-region:\n{conf}"
        );
        // Loopback guard must not trip for a public endpoint.
        assert!(
            !conf.contains("no_check_certificate"),
            "public endpoint must keep cert verification:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_s3_loopback_no_check_certificate() {
        // #2 regression: the Filen Desktop S3 bridge runs on a self-signed
        // loopback endpoint without IP SANs. rclone rejects it unless
        // `no_check_certificate` is set, mirroring AeroFTP's accept-invalid-
        // certs-on-loopback behaviour.
        let servers = vec![RcloneExportServer {
            id: "filen-s3".to_string(),
            name: "filen-s3".to_string(),
            host: String::new(),
            port: 1800,
            username: "FILENKEY".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({
                "endpoint": "https://127.0.0.1:1800",
                "bucket": "filen",
                "verifyCert": false
            })),
            provider_id: Some("filen-desktop-s3".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("filen-s3".to_string(), "filensecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-s3-filen.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("endpoint = https://127.0.0.1:1800"),
            "loopback endpoint must be preserved:\n{conf}"
        );
        assert!(
            conf.contains("no_check_certificate = true"),
            "loopback self-signed bridge must disable cert verification:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_s3_public_endpoint_keeps_cert_check() {
        // Regression guard: a normal public S3 endpoint must keep certificate
        // verification and must not be rewritten.
        let servers = vec![RcloneExportServer {
            id: "aws".to_string(),
            name: "aws".to_string(),
            host: "s3.amazonaws.com".to_string(),
            port: 443,
            username: "AKIAEXAMPLE".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({ "region": "eu-west-1", "bucket": "mybucket" })),
            provider_id: Some("amazon-s3".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("aws".to_string(), "s3secret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-s3-aws-public.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("endpoint = https://s3.amazonaws.com"),
            "host endpoint must be preserved:\n{conf}"
        );
        assert!(
            !conf.contains("no_check_certificate"),
            "public endpoint must keep cert verification:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_rejects_ini_section_injection() {
        // A profile whose host/username/options carry CR/LF must not be able to
        // forge a second [section] in the generated rclone.conf.
        let servers = vec![RcloneExportServer {
            id: "victim".to_string(),
            name: "victim".to_string(),
            host: "h\n[evil]\ntype = local".to_string(),
            port: 21,
            username: "u\r\n[evil2]\ntype = local".to_string(),
            protocol: Some("ftp".to_string()),
            options: Some(serde_json::json!({ "bucket": "b\n[evil3]\ntype = local" })),
            provider_id: None,
        }];
        let mut passwords = HashMap::new();
        passwords.insert("victim".to_string(), "p\n[evil4]\ntype = local".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-ini-injection.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        // The injected text may survive as an inline substring of a value line
        // (e.g. `host = h[evil]type = local`), which is harmless; what must NOT
        // happen is a forged section header or backend line of its own. Check
        // line-anchored.
        for line in conf.lines() {
            let t = line.trim();
            assert!(
                !t.starts_with("[evil"),
                "forged section header must not be its own line: {line:?}\n{conf}"
            );
            assert_ne!(
                t, "type = local",
                "forged backend line must not appear: {line:?}\n{conf}"
            );
        }
        // Exactly one real section header for the single exported remote.
        let headers: Vec<&str> = conf
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with('[') && l.ends_with(']'))
            .collect();
        assert_eq!(
            headers,
            ["[victim]"],
            "exactly one real section header expected:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_b2_key_rejects_ini_injection() {
        // F-05: the ftp test above only exercises obscure_password (base64,
        // newline-free). Backblaze b2 writes the application key verbatim and so
        // MUST run it through ini_value; a CR/LF key must not forge a second
        // [section]. This regresses F-01 (the b2 gap that skipped ini_value).
        let servers = vec![RcloneExportServer {
            id: "victim".to_string(),
            name: "victim".to_string(),
            host: String::new(),
            port: 443,
            username: "account-key-id".to_string(),
            protocol: Some("backblaze".to_string()),
            options: None,
            provider_id: None,
        }];
        let mut passwords = HashMap::new();
        passwords.insert(
            "victim".to_string(),
            "appkey\n[evil]\ntype = local".to_string(),
        );

        let tmp = std::env::temp_dir().join("aeroftp-test-export-ini-injection-b2.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        for line in conf.lines() {
            let t = line.trim();
            assert!(
                !t.starts_with("[evil"),
                "b2 forged section header on its own line: {line:?}\n{conf}"
            );
            assert_ne!(
                t, "type = local",
                "b2 forged backend line: {line:?}\n{conf}"
            );
        }
        let headers: Vec<&str> = conf
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with('[') && l.ends_with(']'))
            .collect();
        assert_eq!(
            headers,
            ["[victim]"],
            "exactly one real section header expected:\n{conf}"
        );
    }

    /// Two profiles of one name (a list saved before names were unique) were
    /// already two remotes, `Lab` and `Lab-2`, but both carried the second
    /// one's password: the map handed to the export was keyed by name.
    #[test]
    fn two_profiles_of_one_name_export_each_with_its_own_password() {
        let (servers, passwords) = crate::bridge_shared::two_profiles_named_lab::<RcloneExportServer>(
            serde_json::json!({"host": "a.example.com", "port": 22, "username": "u", "protocol": "sftp"}),
            serde_json::json!({"host": "b.example.com", "port": 22, "username": "u", "protocol": "sftp"}),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        let outcome = export_rclone(&servers, &passwords, &path).unwrap();
        assert_eq!(outcome.exported, 2, "{:?}", outcome.skipped);
        let back = import_rclone(&path).unwrap();
        assert_eq!(
            crate::bridge_shared::names_hosts_secrets(&back.servers),
            crate::bridge_shared::owned3(&[
                ("Lab", "a.example.com", "secret-a"),
                ("Lab-2", "b.example.com", "secret-b"),
            ])
        );
    }

    // ---- 2026-11-06 live round trip: 118 profiles through rclone v1.75.2 ----

    fn server_with(
        name: &str,
        protocol: &str,
        host: &str,
        port: u32,
        provider_id: Option<&str>,
        options: serde_json::Value,
    ) -> RcloneExportServer {
        RcloneExportServer {
            id: name.to_string(),
            name: name.to_string(),
            host: host.to_string(),
            port,
            username: "alice".to_string(),
            protocol: Some(protocol.to_string()),
            options: Some(options),
            provider_id: provider_id.map(str::to_string),
        }
    }

    fn section_of<'a>(conf: &'a str, name: &str) -> &'a str {
        let header = format!("[{name}]\n");
        let start = conf
            .find(&header)
            .unwrap_or_else(|| panic!("no [{name}]:\n{conf}"));
        let body = &conf[start + header.len()..];
        &body[..body.find("\n[").map(|i| i + 1).unwrap_or(body.len())]
    }

    fn import_section(conf: &str, name: &str) -> MappedProfile {
        let sections = parse_rclone_conf(conf);
        map_remote(name, &sections[name], None).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    fn option_of<'a>(profile: &'a MappedProfile, key: &str) -> Option<&'a serde_json::Value> {
        profile.options.as_ref().and_then(|o| o.get(key))
    }

    #[test]
    fn a_profile_that_skips_certificate_checks_keeps_skipping_them_in_rclone_and_back() {
        let off = |extra: serde_json::Value| {
            let mut o = serde_json::json!({ "verifyCert": false });
            o.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            o
        };
        let servers = vec![
            server_with(
                "ftps",
                "ftp",
                "ftp.example.test",
                21,
                None,
                off(serde_json::json!({ "tlsMode": "explicit" })),
            ),
            server_with(
                "dav",
                "webdav",
                "dav.example.test",
                443,
                None,
                off(serde_json::json!({})),
            ),
            server_with(
                "obj",
                "s3",
                "s3.example.test",
                443,
                Some("custom-s3"),
                off(serde_json::json!({ "endpoint": "https://s3.example.test" })),
            ),
            server_with(
                "sw",
                "swift",
                "https://auth.example.test/v3",
                443,
                None,
                off(serde_json::json!({})),
            ),
        ];
        let (outcome, conf) = export_to_string(&servers, "cert-off");
        assert_eq!(outcome.exported, 4, "{:?}\n{conf}", outcome.skipped);
        // The FTP backend has the option itself; the others only through the
        // `override.` form, since rclone ignores the bare key there.
        assert!(
            section_of(&conf, "ftps").contains("\nno_check_certificate = true\n"),
            "{conf}"
        );
        for name in ["dav", "obj", "sw"] {
            let section = section_of(&conf, name);
            assert!(
                section.contains("override.no_check_certificate = true"),
                "{name}:\n{conf}"
            );
            assert!(
                !section.contains("\nno_check_certificate"),
                "{name}: bare key is ignored by rclone:\n{conf}"
            );
        }
        for name in ["ftps", "dav", "obj", "sw"] {
            assert_eq!(
                option_of(&import_section(&conf, name), "verifyCert"),
                Some(&serde_json::Value::Bool(false)),
                "{name} must come back with verification off"
            );
        }
    }

    #[test]
    fn verification_left_on_writes_no_certificate_switch() {
        let servers = vec![
            server_with(
                "ftps",
                "ftp",
                "ftp.example.test",
                21,
                None,
                serde_json::json!({ "tlsMode": "explicit" }),
            ),
            // Cleartext FTP has no certificate to skip.
            server_with(
                "plain",
                "ftp",
                "ftp.example.test",
                21,
                None,
                serde_json::json!({ "verifyCert": false }),
            ),
            server_with(
                "dav",
                "webdav",
                "dav.example.test",
                443,
                None,
                serde_json::json!({}),
            ),
        ];
        let (_, conf) = export_to_string(&servers, "cert-on");
        assert!(!conf.contains("no_check_certificate"), "{conf}");
        assert_eq!(
            option_of(&import_section(&conf, "ftps"), "verifyCert"),
            None
        );
    }

    #[test]
    fn a_swift_profile_exports_the_auth_url_the_provider_dials() {
        let servers = vec![
            // Blomp's preset keeps the auth URL as the host, which is where
            // `SwiftConfig::from_provider_config` reads it.
            server_with(
                "blomp",
                "swift",
                "https://authenticate.blomp.com",
                443,
                Some("blomp"),
                serde_json::json!({}),
            ),
            server_with(
                "bare",
                "swift",
                "keystone.example.test",
                443,
                None,
                serde_json::json!({}),
            ),
            // No host: the URL the profile carries in `options.endpoint`.
            server_with(
                "opt",
                "swift",
                "",
                443,
                None,
                serde_json::json!({ "endpoint": "https://id.example.test/v3" }),
            ),
            server_with("none", "swift", "", 443, None, serde_json::json!({})),
        ];
        let (outcome, conf) = export_to_string(&servers, "swift-auth");
        // rclone refuses an auth URL with no version in it, where AeroFTP
        // probes; Blomp is Keystone v2 under the fixed tenant `storage`.
        let blomp_section = section_of(&conf, "blomp");
        assert!(
            blomp_section.contains("auth = https://authenticate.blomp.com/v2.0\n"),
            "{conf}"
        );
        assert!(blomp_section.contains("tenant = storage\n"), "{conf}");
        assert!(
            section_of(&conf, "bare").contains("auth = https://keystone.example.test\n"),
            "{conf}"
        );
        assert!(!section_of(&conf, "bare").contains("tenant ="), "{conf}");
        // An unversioned root that is not Blomp cannot be guessed: say so.
        assert!(
            conf.contains("'bare': the auth URL names no Keystone version"),
            "{conf}"
        );
        assert!(
            !conf.contains("'opt': the auth URL names no Keystone version"),
            "{conf}"
        );
        assert!(
            section_of(&conf, "opt").contains("auth = https://id.example.test/v3\n"),
            "{conf}"
        );
        assert!(!conf.contains("[none]"), "{conf}");
        assert!(
            outcome
                .skipped
                .iter()
                .any(|s| s.name == "none" && s.reason.contains("auth URL")),
            "{:?}",
            outcome.skipped
        );
        // And it imports, as Blomp, where it used to be refused for "no auth URL".
        let blomp = import_section(&conf, "blomp");
        assert_eq!(blomp.provider_id.as_deref(), Some("blomp"));
        assert_eq!(
            import_section(&conf, "bare").provider_id.as_deref(),
            Some("custom-swift")
        );
    }

    #[test]
    fn a_profile_without_a_host_is_skipped_with_the_reason_not_written_as_https_colon() {
        for proto in ["webdav", "ftp", "ftps", "sftp"] {
            let servers = vec![server_with(
                "nohost",
                proto,
                "  ",
                443,
                None,
                serde_json::json!({}),
            )];
            let (outcome, conf) = export_to_string(&servers, &format!("nohost-{proto}"));
            assert_eq!(outcome.exported, 0, "{proto}:\n{conf}");
            assert!(!conf.contains("[nohost]"), "{proto}:\n{conf}");
            assert!(
                outcome.skipped[0].reason.contains("no host"),
                "{proto}: {:?}",
                outcome.skipped
            );
        }
    }

    #[test]
    fn webdav_alias_path_is_relative_to_the_collection_the_url_opens() {
        let nc = "https://cloud.example.test/remote.php/dav/files/alice/";
        assert_eq!(
            webdav_alias_path(nc, "/remote.php/dav/files/{username}/", "alice"),
            ""
        );
        assert_eq!(
            webdav_alias_path(nc, "/remote.php/dav/files/alice", "alice"),
            ""
        );
        assert_eq!(
            webdav_alias_path(nc, "/remote.php/dav/files/{username}/Photos/2026", "alice"),
            "Photos/2026"
        );
        assert_eq!(
            webdav_alias_path("https://dav.example.test", "/docs/", "alice"),
            "docs"
        );
        assert_eq!(
            webdav_alias_path("https://dav.example.test:8443/dav", "/team", "alice"),
            "team"
        );
    }

    #[test]
    fn a_nextcloud_profile_starting_in_its_own_collection_carries_no_alias() {
        // TAB.DIGITAL stores the collection, `{username}` included, as both the
        // URL path and the start folder; the lab Nextcloud stores a bare host.
        let servers = vec![
            server_with(
                "tab",
                "webdav",
                "https://fie.example.test/remote.php/dav/files/{username}/",
                443,
                Some("tabdigital"),
                serde_json::json!({ "initial_path": "/remote.php/dav/files/{username}/" }),
            ),
            server_with(
                "lab",
                "webdav",
                "https://cloud.example.test",
                443,
                Some("nextcloud"),
                serde_json::json!({ "initial_path": "/remote.php/dav/files/alice/" }),
            ),
            server_with(
                "photos",
                "webdav",
                "https://cloud.example.test",
                443,
                Some("nextcloud"),
                serde_json::json!({ "initial_path": "/remote.php/dav/files/alice/Photos" }),
            ),
        ];
        let (_, conf) = export_to_string(&servers, "dav-alias");
        assert!(!conf.contains("[tab-path]"), "{conf}");
        assert!(!conf.contains("[lab-path]"), "{conf}");
        assert!(
            !conf.contains("{username}"),
            "the template must be resolved:\n{conf}"
        );
        assert!(
            section_of(&conf, "photos-path").contains("remote = photos:Photos\n"),
            "{conf}"
        );
    }

    #[test]
    fn an_absolute_crypt_scope_is_taken_relative_to_the_start_folder_alias() {
        let crypt = |scope: &str| {
            server_with(
                "nas",
                "sftp",
                "nas.example.test",
                22,
                None,
                serde_json::json!({
                    "initial_path": "/home/u/vault",
                    "rcloneCryptEnabled": true,
                    "rcloneCryptOverlayScope": scope,
                    "rcloneCryptPassword": "pw"
                }),
            )
        };
        // The app persists the scope absolute (`resolveOverlayScope`).
        let (_, conf) = export_to_string(&[crypt("/home/u/vault")], "crypt-scope-root");
        assert!(conf.contains("remote = nas-path:\n"), "{conf}");
        let (_, conf) = export_to_string(&[crypt("/home/u/vault/enc")], "crypt-scope-sub");
        assert!(conf.contains("remote = nas-path:/enc\n"), "{conf}");
        assert!(!conf.contains("/home/u/vault/home/u/vault"), "{conf}");
        assert!(
            !conf.contains("nas-path:/home/u/vault"),
            "the folder inside itself:\n{conf}"
        );
    }

    #[test]
    fn an_sftp_crypt_under_the_login_folder_opens_where_the_profile_does() {
        // The app saves the scope from `~/projects` as `/~/projects`
        // (`resolveOverlayScope`); the alias holds `projects`.
        let crypt = |start: &str, scope: &str| {
            server_with(
                "nas",
                "sftp",
                "nas.example.test",
                22,
                None,
                serde_json::json!({
                    "initial_path": start,
                    "rcloneCryptEnabled": true,
                    "rcloneCryptOverlayScope": scope,
                    "rcloneCryptPassword": "pw"
                }),
            )
        };
        let remote_of = |start: &str, scope: &str| {
            let (_, conf) = export_to_string(&[crypt(start, scope)], "crypt-login-folder");
            conf.lines()
                .skip_while(|line| *line != "type = crypt")
                .find_map(|line| line.strip_prefix("remote = "))
                .unwrap_or_else(|| panic!("no crypt remote:\n{conf}"))
                .to_string()
        };
        assert_eq!(remote_of("~/projects", "/~/projects"), "nas-path:");
        assert_eq!(
            remote_of("~/projects", "/~/projects/vault"),
            "nas-path:/vault"
        );
        assert_eq!(
            remote_of("~/projects", "/vault"),
            "nas-path:/vault",
            "an older scope relative to the start folder"
        );
        // `~` alone: no alias, the crypt wraps the login folder itself.
        assert_eq!(remote_of("~", "/~"), "nas:");
        assert_eq!(remote_of("~", "/~/vault"), "nas:vault");
        assert_eq!(sftp_login_relative_scope("/~x/vault"), None);
        assert_eq!(sftp_login_relative_scope("/srv/~"), None);
    }

    #[test]
    fn a_webdav_crypt_scope_saved_on_the_server_path_is_taken_from_the_collection() {
        // The app saves the scope as a server path; rclone's remote and the
        // start-folder alias begin at the collection the URL opens.
        let crypt = |scope: &str| {
            server_with(
                "dav",
                "webdav",
                "https://cloud.example.test",
                443,
                Some("nextcloud"),
                serde_json::json!({
                    "initial_path": "/remote.php/dav/files/alice/docs",
                    "rcloneCryptEnabled": true,
                    "rcloneCryptOverlayScope": scope,
                    "rcloneCryptPassword": "pw"
                }),
            )
        };
        let (_, conf) = export_to_string(
            &[crypt("/remote.php/dav/files/alice/docs")],
            "dav-crypt-root",
        );
        assert!(
            section_of(&conf, "dav-path").contains("remote = dav:docs\n"),
            "{conf}"
        );
        assert!(conf.contains("remote = dav-path:\n"), "{conf}");
        let (_, conf) = export_to_string(
            &[crypt("/remote.php/dav/files/alice/docs/vault")],
            "dav-crypt-sub",
        );
        assert!(conf.contains("remote = dav-path:/vault\n"), "{conf}");
        assert!(
            !conf.contains("dav-path:/remote.php"),
            "the collection written twice:\n{conf}"
        );
    }

    #[test]
    fn an_sftp_root_start_folder_survives_the_round_trip() {
        // AeroFTP opens `/` at the server root and no start folder at the
        // login folder; rclone's `nas:` is the login folder, `nas:/` the root.
        let servers = vec![
            server_with(
                "root",
                "sftp",
                "nas.example.test",
                22,
                None,
                serde_json::json!({ "initial_path": "/" }),
            ),
            server_with(
                "home",
                "sftp",
                "nas.example.test",
                22,
                None,
                serde_json::json!({}),
            ),
        ];
        let (_, conf) = export_to_string(&servers, "sftp-root");
        assert!(
            section_of(&conf, "root-path").contains("remote = root:/\n"),
            "{conf}"
        );
        assert!(!conf.contains("[home-path]"), "{conf}");
        let path = tmp_write(
            &conf,
            &format!("aeroftp-test-sftp-root-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).expect("import");
        std::fs::remove_file(&path).ok();
        let initial = |name: &str| {
            result
                .servers
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .initial_path
                .clone()
        };
        assert_eq!(initial("root").as_deref(), Some("/"));
        assert_eq!(initial("home"), None);

        // A foreign alias at the root says the same.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        std::fs::write(
            &path,
            "[nas]\ntype = sftp\nhost = nas.example.test\nuser = u\n\n[top]\ntype = alias\nremote = nas:/\n",
        )
        .unwrap();
        let result = import_rclone(&path).expect("import");
        let top = result.servers.iter().find(|s| s.name == "top").unwrap();
        assert_eq!(top.initial_path.as_deref(), Some("/"));
    }

    #[test]
    fn a_b2_bucket_travels_as_the_alias_path_both_ways() {
        let servers = vec![server_with(
            "b2",
            "backblaze",
            "api.backblazeb2.com",
            443,
            Some("backblaze-native"),
            serde_json::json!({ "bucket": "media" }),
        )];
        let (_, conf) = export_to_string(&servers, "b2-bucket");
        assert!(
            section_of(&conf, "b2-media").contains("type = alias\nremote = b2:media\n"),
            "{conf}"
        );
        let path = tmp_write(
            &conf,
            &format!("aeroftp-test-b2-bucket-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).expect("import");
        std::fs::remove_file(&path).ok();
        let names: Vec<&str> = result.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["b2"], "the alias folds back: {names:?}");
        let bucket_of = |server: &ServerProfileExport| {
            server
                .options
                .as_ref()
                .and_then(|o| o.get("bucket"))
                .and_then(|b| b.as_str())
                .map(str::to_string)
        };
        assert_eq!(bucket_of(&result.servers[0]).as_deref(), Some("media"));

        // A foreign alias into a bucket: the first segment is the bucket.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        std::fs::write(
            &path,
            "[b2]\ntype = b2\naccount = a\nkey = k\n\n[photos]\ntype = alias\nremote = b2:media/photos\n",
        )
        .unwrap();
        let result = import_rclone(&path).expect("import");
        let photos = result.servers.iter().find(|s| s.name == "photos").unwrap();
        assert_eq!(bucket_of(photos).as_deref(), Some("media"));
        assert_eq!(photos.initial_path.as_deref(), Some("/photos"));
    }

    /// Every `[section]` header of an exported file.
    fn section_headers(conf: &str) -> Vec<&str> {
        conf.lines()
            .filter_map(|l| l.strip_prefix('[').and_then(|l| l.strip_suffix(']')))
            .collect()
    }

    #[test]
    fn a_dotted_name_never_takes_another_profile_name() {
        // `Lab.x` gives up its dot, but `Lab-x` is a real profile: taken as it
        // was, the real one moved to `Lab-x-2`.
        let lab = |name: &str, host: &str| {
            server_with(name, "ftp", host, 21, None, serde_json::json!({}))
        };
        let servers = vec![
            lab("Lab", "lab.test"),
            lab("Lab.x", "dotted.test"),
            lab("Lab-x", "real.test"),
        ];
        let (_, conf) = export_to_string(&servers, "dotted-reserved");
        assert!(
            section_of(&conf, "Lab-x").contains("host = real.test"),
            "{conf}"
        );
        assert!(
            !section_of(&conf, "Lab-x").contains("dotted.test"),
            "{conf}"
        );
        assert!(conf.contains("host = dotted.test"), "{conf}");

        // An overlay name may not become the ancestor of a profile written
        // later: `vault` before `vault.team` renamed that profile.
        let servers = vec![
            export_server(
                "nas",
                "ftp",
                Some(serde_json::json!({
                    "rcloneCryptEnabled": true,
                    "rcloneCryptOverlayName": "vault",
                    "rcloneCryptPassword": "topsecret",
                })),
            ),
            export_server("vault.team", "ftp", None),
        ];
        let (outcome, conf) = export_to_string(&servers, "dotted-overlay");
        assert!(conf.contains("[vault.team]\ntype = ftp"), "{conf}");
        assert!(!conf.contains("[vault]\n"), "{conf}");
        assert!(
            outcome.skipped.iter().any(|s| s.name == "nas"),
            "{:?}",
            outcome.skipped
        );
    }

    #[test]
    fn no_exported_section_reads_another_through_a_dotted_name() {
        // Codex review RCB-01: rclone reads a key `[Lab.x]` lacks from
        // `[Lab]`, so the child logged in to its own host with the parent's
        // password. No two sections may be ancestor and descendant, in either
        // order of the profiles, and a generated alias may not become one.
        let lab = |name: &str, host: &str| {
            let mut s = server_with(name, "ftp", host, 21, None, serde_json::json!({}));
            s.id = format!("{name}-id");
            s
        };
        for servers in [
            vec![
                lab("Lab", "legit.test"),
                lab("Lab.attacker", "attacker.test"),
            ],
            vec![
                lab("Lab.attacker", "attacker.test"),
                lab("Lab", "legit.test"),
            ],
            vec![
                lab("nas", "nas.test"),
                server_with(
                    "nas-path.x",
                    "ftp",
                    "x.test",
                    21,
                    None,
                    serde_json::json!({}),
                ),
                server_with(
                    "nas",
                    "sftp",
                    "nas.test",
                    22,
                    None,
                    serde_json::json!({ "initial_path": "/data" }),
                ),
            ],
        ] {
            let (_, conf) = export_with_passwords(&servers, &[("Lab-id", "OWNER")], "dotted");
            let headers = section_headers(&conf);
            for a in &headers {
                for b in &headers {
                    assert!(
                        !b.starts_with(&format!("{a}.")),
                        "[{b}] would read the keys it lacks from [{a}]:\n{conf}"
                    );
                }
            }
        }
        let (_, conf) = export_with_passwords(
            &[
                lab("Lab", "legit.test"),
                lab("Lab.attacker", "attacker.test"),
            ],
            &[("Lab-id", "OWNER")],
            "dotted-name",
        );
        assert!(
            section_of(&conf, "Lab-attacker").contains("host = attacker.test\n"),
            "{conf}"
        );
        assert!(
            !section_of(&conf, "Lab-attacker").contains("pass ="),
            "{conf}"
        );
        // A dotted name with no section above it keeps its dots.
        let (_, conf) = export_to_string(&[lab("ftp.example.com", "h.test")], "dotted-alone");
        assert!(conf.contains("[ftp.example.com]\n"), "{conf}");
    }

    #[test]
    fn an_import_that_would_expand_past_the_budget_is_refused() {
        // Codex review RCB-02: every alias is mapped from the section it opens,
        // so its secrets are copied once per alias.
        let mut sections: HashMap<String, RcloneRemote> = HashMap::new();
        let mut base = RcloneRemote::new();
        base.insert("type".into(), "s3".into());
        base.insert("secret_access_key".into(), "A".repeat(1024 * 1024));
        sections.insert("base".into(), base);
        let alias = |n: usize, sections: &mut HashMap<String, RcloneRemote>| {
            for i in 0..n {
                let mut a = RcloneRemote::new();
                a.insert("type".into(), "alias".into());
                a.insert("remote".into(), "base:".into());
                sections.insert(format!("a{i}"), a);
            }
        };
        alias(40, &mut sections);
        assert!(check_rclone_expansion(&sections).is_ok());
        alias(80, &mut sections);
        let err = check_rclone_expansion(&sections).unwrap_err();
        assert!(err.contains("expands to more than"), "{err}");
    }

    #[test]
    fn an_obscured_secret_keeps_its_line_breaks_through_the_export() {
        // Codex review RCB-04: the CR/LF strip for raw INI values changed the
        // crypt key material, and the key passphrase, before obscuring.
        let servers = vec![server_with(
            "demo",
            "sftp",
            "example.test",
            22,
            None,
            serde_json::json!({
                "private_key_path": "/keys/demo",
                "key_passphrase": "pass\nphrase",
                "rcloneCryptEnabled": true,
                "rcloneCryptPassword": "first\nsecond",
                "rcloneCryptPassword2": "salt\r\nvalue"
            }),
        )];
        let (_, conf) = export_to_string(&servers, "obscured-crlf");
        let value = |section: &str, key: &str| {
            section_of(&conf, section)
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{key} = ")))
                .map(|v| reveal_obscured(v).expect("reveal"))
        };
        assert_eq!(
            value("demo-crypt", "password").as_deref(),
            Some("first\nsecond")
        );
        assert_eq!(
            value("demo-crypt", "password2").as_deref(),
            Some("salt\r\nvalue")
        );
        assert_eq!(
            value("demo", "key_file_pass").as_deref(),
            Some("pass\nphrase")
        );
        // The file still has one key per line.
        assert!(conf.lines().all(|l| !l.contains('\r')), "{conf}");
    }

    #[test]
    fn a_crypt_over_an_oauth_remote_keeps_the_app_of_the_remote_it_opens() {
        let password = obscure_password("pw").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        std::fs::write(
            &path,
            format!(
                "[z-drive]\ntype = drive\nclient_id = APP_A\nclient_secret = SECRET_A\n\
                 token = {{\"access_token\":\"AT\",\"refresh_token\":\"RT\",\"expiry\":\"2030-01-01T00:00:00Z\"}}\n\n\
                 [a-vault]\ntype = crypt\nremote = z-drive:\npassword = {password}\n"
            ),
        )
        .unwrap();
        let result = import_rclone(&path).unwrap();
        let id = result_app(&result, "a-vault");
        let secrets = result.provider_secrets.get(&id).expect("the app");
        assert_eq!(secrets.oauth_client_id.as_deref(), Some("APP_A"));
        assert_eq!(secrets.oauth_client_secret.as_deref(), Some("SECRET_A"));
    }

    #[test]
    fn a_key_file_that_expands_past_path_max_refuses_the_import() {
        let key = "${RCLONE_CONFIG_DIR}".repeat(1000);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        std::fs::write(
            &path,
            format!("[nas]\ntype = sftp\nhost = nas.example.test\nuser = u\nkey_file = {key}\n"),
        )
        .unwrap();
        let err = import_rclone(&path).err().expect("refused");
        assert!(err.contains("key_file of 'nas' expands past"), "{err}");
    }

    #[test]
    fn crypt_remotes_without_a_colon_count_against_the_expansion_budget() {
        // `remote = base` opens all of `base`, so each crypt copies its secret.
        let secret = "k".repeat(1024 * 1024);
        let mut conf =
            format!("[base]\ntype = s3\nprovider = AWS\nsecret_access_key = {secret}\n\n");
        for i in 0..70 {
            conf.push_str(&format!("[c{i}]\ntype = crypt\nremote = base\n\n"));
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        std::fs::write(&path, conf).unwrap();
        let err = import_rclone(&path).err().expect("refused");
        assert!(err.contains("expands to more than"), "{err}");
    }

    #[test]
    fn an_import_larger_than_the_cap_is_refused_while_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        let cap = crate::bridge_shared::MAX_IMPORT_FILE_BYTES as usize;
        std::fs::write(&path, "#".repeat(cap + 1)).unwrap();
        let err = import_rclone(&path).err().expect("refused");
        assert!(err.contains("larger than"), "{err}");
    }

    #[test]
    fn import_keeps_the_fields_its_own_export_writes() {
        let key_pass = obscure_password("key-pass").expect("obscure");
        let conf = format!(
            "[key]\ntype = sftp\nhost = h.example.test\nuser = u\nkey_file = ~/.ssh/id_ed25519\nkey_file_pass = {key_pass}\n\n\
             [od]\ntype = onedrive\ndrive_id = D980DD4FB1784A97\ndrive_type = personal\nregion = global\n\n\
             [ali]\ntype = s3\nprovider = Alibaba\nendpoint = https://oss-eu-central-1.aliyuncs.com\naccess_key_id = a\nsecret_access_key = b\n\n\
             [tc]\ntype = s3\nprovider = TencentCOS\nendpoint = https://cos.ap-guangzhou.myqcloud.com\naccess_key_id = a\nsecret_access_key = b\n\n\
             [gcs]\ntype = s3\nprovider = GCS\nendpoint = https://storage.googleapis.com\naccess_key_id = a\nsecret_access_key = b\n"
        );
        let key = import_section(&conf, "key");
        assert_eq!(
            option_of(&key, "private_key_path").and_then(|v| v.as_str()),
            Some("~/.ssh/id_ed25519")
        );
        assert_eq!(
            option_of(&key, "key_passphrase").and_then(|v| v.as_str()),
            Some("key-pass")
        );
        let od = import_section(&conf, "od");
        for (k, v) in [
            ("drive_id", "D980DD4FB1784A97"),
            ("drive_type", "personal"),
            ("region", "global"),
        ] {
            assert_eq!(option_of(&od, k).and_then(|x| x.as_str()), Some(v), "{k}");
        }
        assert_eq!(
            import_section(&conf, "ali").provider_id.as_deref(),
            Some("alibaba-oss")
        );
        assert_eq!(
            import_section(&conf, "tc").provider_id.as_deref(),
            Some("tencent-cos")
        );
        assert_eq!(
            import_section(&conf, "gcs").provider_id.as_deref(),
            Some("google-cloud-storage")
        );
    }

    #[test]
    fn an_aeroftp_export_comes_back_opening_where_each_profile_opened() {
        let servers = vec![
            server_with(
                "nas",
                "sftp",
                "nas.example.test",
                22,
                None,
                serde_json::json!({ "initial_path": "/home/u/team" }),
            ),
            server_with(
                "site",
                "ftp",
                "ftp.example.test",
                21,
                None,
                serde_json::json!({ "initial_path": "/www/site" }),
            ),
            server_with(
                "obj",
                "s3",
                "s3.example.test",
                443,
                Some("custom-s3"),
                serde_json::json!({ "endpoint": "https://s3.example.test", "bucket": "media" }),
            ),
        ];
        let (outcome, conf) = export_to_string(&servers, "alias-fold");
        assert_eq!(outcome.profiles, 3, "{conf}");
        assert_eq!(outcome.exported, 6, "each profile and its alias:\n{conf}");
        assert!(
            conf.contains("type = alias"),
            "the export carries the folders as aliases:\n{conf}"
        );
        let path = tmp_write(
            &conf,
            &format!("aeroftp-test-alias-fold-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).expect("import");
        std::fs::remove_file(&path).ok();
        let names: Vec<&str> = result.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["nas", "obj", "site"],
            "one profile each, no alias profiles: {names:?}"
        );
        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
        let by = |n: &str| result.servers.iter().find(|s| s.name == n).unwrap();
        assert_eq!(by("nas").initial_path.as_deref(), Some("/home/u/team"));
        assert_eq!(by("site").initial_path.as_deref(), Some("/www/site"));
        assert_eq!(
            by("obj")
                .options
                .as_ref()
                .and_then(|o| o.get("bucket"))
                .and_then(|b| b.as_str()),
            Some("media")
        );
    }

    #[test]
    fn a_pinned_container_reaches_rclone_through_an_alias_and_comes_back_pinned() {
        // rclone's swift and azureblob backends have no `container` key: the
        // container a profile opens has to be the path of an alias.
        let servers = vec![
            server_with(
                "blomp",
                "swift",
                "https://authenticate.blomp.com",
                443,
                Some("blomp"),
                serde_json::json!({}),
            ),
            server_with(
                "os",
                "swift",
                "https://id.example.test/v3",
                443,
                None,
                serde_json::json!({ "bucket": "media" }),
            ),
            server_with(
                "az",
                "azure",
                "acct.blob.core.windows.net",
                443,
                None,
                serde_json::json!({ "bucket": "backups" }),
            ),
            server_with(
                "os-root",
                "swift",
                "https://id.example.test/v3",
                443,
                None,
                serde_json::json!({}),
            ),
        ];
        let (_, conf) = export_to_string(&servers, "containers");
        // Blomp's one container is named after the login (`discover_container`).
        assert!(
            section_of(&conf, "blomp-alice").contains("type = alias\nremote = blomp:alice\n"),
            "{conf}"
        );
        assert!(
            section_of(&conf, "os-media").contains("remote = os:media\n"),
            "{conf}"
        );
        assert!(
            section_of(&conf, "az-backups").contains("remote = az:backups\n"),
            "{conf}"
        );
        // No container pinned, no alias: the remote lists its containers.
        assert!(!conf.contains("[os-root-"), "{conf}");

        let path = tmp_write(
            &conf,
            &format!("aeroftp-test-containers-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).expect("import");
        std::fs::remove_file(&path).ok();
        let mut names: Vec<&str> = result.servers.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["az", "blomp", "os", "os-root"],
            "the container aliases fold back into their profile: {names:?}"
        );
        for (name, container) in [("os", "media"), ("az", "backups"), ("blomp", "alice")] {
            let server = result.servers.iter().find(|s| s.name == name).unwrap();
            assert_eq!(
                server
                    .options
                    .as_ref()
                    .and_then(|o| o.get("bucket"))
                    .and_then(|b| b.as_str()),
                Some(container),
                "{name}: {:?}",
                server.options
            );
            // The alias path is the container itself: nothing below it to open.
            assert_eq!(server.initial_path, None, "{name}");
        }
    }

    #[test]
    fn a_foreign_alias_path_names_the_container_even_over_a_container_line() {
        // rclone ignores `container =` on swift: `os:media/photos` opens the
        // container `media`, whatever the base section says.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        std::fs::write(
            &path,
            "[os]\ntype = swift\nauth = https://id.example.test/v3\nuser = u\nkey = k\ncontainer = other\n\n\
             [photos]\ntype = alias\nremote = os:media/photos\n",
        )
        .unwrap();
        let result = import_rclone(&path).expect("import");
        let server = result.servers.iter().find(|s| s.name == "photos").unwrap();
        assert_eq!(
            server
                .options
                .as_ref()
                .and_then(|o| o.get("bucket"))
                .and_then(|b| b.as_str()),
            Some("media")
        );
        assert_eq!(server.initial_path.as_deref(), Some("/photos"));
    }

    #[test]
    fn an_alias_over_an_oauth_remote_keeps_the_app_of_the_remote_it_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rclone.conf");
        std::fs::write(
            &path,
            "[gd]\ntype = drive\nclient_id = my-app\nclient_secret = my-secret\n\
             token = {\"access_token\":\"a\",\"refresh_token\":\"r\",\"expiry\":\"2030-01-01T00:00:00Z\"}\n\n\
             [team]\ntype = alias\nremote = gd:Team\n",
        )
        .unwrap();
        let result = import_rclone(&path).unwrap();
        let id = result_app(&result, "team");
        let secrets = result.provider_secrets.get(&id).expect("the app");
        assert_eq!(secrets.oauth_client_id.as_deref(), Some("my-app"));
        assert_eq!(secrets.oauth_client_secret.as_deref(), Some("my-secret"));
    }

    #[test]
    fn a_local_bridge_is_exported_on_the_scheme_its_app_serves() {
        let servers = vec![
            // Filen Desktop's WebDAV bridge: no scheme saved, AeroFTP resolves
            // it at connect; the export cannot probe and takes plain HTTP.
            server_with(
                "filen-bridge",
                "webdav",
                "local.webdav.filen.io",
                1900,
                Some("filen-desktop-webdav"),
                serde_json::json!({}),
            ),
            // A scheme saved on a bridge profile is not what AeroFTP dials
            // (it resolves the bridge at connect), so it does not decide.
            server_with(
                "filen-saved",
                "webdav",
                "local.webdav.filen.io",
                1900,
                Some("filen-desktop-webdav"),
                serde_json::json!({ "webdavScheme": "https" }),
            ),
            // Off the bridges, the saved scheme decides.
            server_with(
                "lan-http",
                "webdav",
                "nas.example.test",
                8080,
                None,
                serde_json::json!({ "webdavScheme": "http" }),
            ),
            // A LAN host on the bridge port is not a bridge.
            server_with(
                "lan",
                "webdav",
                "nas.example.test",
                1900,
                None,
                serde_json::json!({}),
            ),
        ];
        let (_, conf) = export_to_string(&servers, "bridge-scheme");
        assert!(
            section_of(&conf, "filen-bridge").contains("url = http://local.webdav.filen.io:1900\n"),
            "{conf}"
        );
        assert!(
            section_of(&conf, "filen-saved").contains("url = http://local.webdav.filen.io:1900\n"),
            "{conf}"
        );
        assert!(
            conf.contains("# 'filen-bridge': local bridge on plain HTTP"),
            "the note says where to switch to TLS:\n{conf}"
        );
        assert!(
            section_of(&conf, "lan-http").contains("url = http://nas.example.test:8080\n"),
            "{conf}"
        );
        assert!(
            section_of(&conf, "lan").contains("url = https://nas.example.test:1900\n"),
            "{conf}"
        );
    }

    #[test]
    fn an_alias_in_someone_elses_config_is_a_profile_that_opens_in_its_folder() {
        let conf = "[nas]\ntype = sftp\nhost = nas.example.test\nuser = u\n\n\
                    [team]\ntype = alias\nremote = nas:/srv/team\n\n\
                    [docs]\ntype = alias\nremote = team:docs\n\n\
                    [home]\ntype = alias\nremote = nas:projects\n\n\
                    [store]\ntype = s3\nprovider = Other\nendpoint = https://s3.example.test\naccess_key_id = a\nsecret_access_key = b\n\n\
                    [photos]\ntype = alias\nremote = store:pics/2026\n\n\
                    [local]\ntype = alias\nremote = /mnt/backup\n";
        let path = tmp_write(
            conf,
            &format!("aeroftp-test-alias-foreign-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).expect("import");
        std::fs::remove_file(&path).ok();
        let by = |n: &str| {
            result
                .servers
                .iter()
                .find(|s| s.name == n)
                .unwrap_or_else(|| panic!("{n}: {:?}", result.skipped))
        };
        assert_eq!(by("nas").initial_path, None);
        assert_eq!(by("team").initial_path.as_deref(), Some("/srv/team"));
        assert_eq!(by("team").host, "nas.example.test");
        // A chain resolves to the remote at its end.
        assert_eq!(by("docs").initial_path.as_deref(), Some("/srv/team/docs"));
        // rclone's sftp reads a relative path from the login folder.
        // AeroFTP opens a start folder without `/` from the root, so it is
        // written under the login folder in the form the provider resolves.
        assert_eq!(by("home").initial_path.as_deref(), Some("~/projects"));
        let photos = by("photos");
        assert_eq!(
            photos
                .options
                .as_ref()
                .and_then(|o| o.get("bucket"))
                .and_then(|b| b.as_str()),
            Some("pics")
        );
        assert_eq!(photos.initial_path.as_deref(), Some("/2026"));
        let local = result
            .skipped
            .iter()
            .find(|s| s.name == "local")
            .expect("local alias skipped");
        assert!(local.reason.contains("local folder"), "{}", local.reason);
        assert!(!result
            .skipped
            .iter()
            .any(|s| s.reason.contains("unsupported rclone type: alias")));
    }
    // ---- the published backend count is the code's (2026-11-06) ----

    /// The body of a top-level fn in this file, from its signature to the next
    /// top-level item.
    fn source_fn_body(signature: &str) -> &'static str {
        let src = include_str!("rclone_import.rs");
        let start = src
            .find(signature)
            .unwrap_or_else(|| panic!("no {signature}"));
        let rest = &src[start..];
        let end = rest[1..]
            .find("\nfn ")
            .into_iter()
            .chain(rest[1..].find("\npub fn "))
            .chain(rest[1..].find("\n#[cfg(test)]"))
            .min()
            .map_or(rest.len(), |e| e + 1);
        &rest[..end]
    }

    #[test]
    fn the_backend_list_is_what_the_export_writes() {
        let body = source_fn_body("pub fn export_rclone(");
        let mut written: Vec<&str> = body
            .split("push_str(\"type = ")
            .skip(1)
            .filter_map(|rest| rest.split(|c: char| !c.is_ascii_alphanumeric()).next())
            .filter(|t| *t != "alias")
            .collect();
        written.sort_unstable();
        written.dedup();
        let mut listed: Vec<&str> = RCLONE_BRIDGE_BACKENDS
            .iter()
            .filter(|b| b.exported)
            .map(|b| b.rclone_type)
            .collect();
        listed.sort_unstable();
        assert_eq!(
            written, listed,
            "export_rclone and RCLONE_BRIDGE_BACKENDS disagree"
        );
    }

    #[test]
    fn the_backend_list_is_what_the_import_reads() {
        let body = source_fn_body("fn map_remote(");
        let match_start = body
            .find("match rclone_type.as_str()")
            .expect("the type match");
        let mut read: Vec<&str> = body[match_start..]
            .lines()
            // The type arms sit at one indentation; the first name is
            // rclone's, the others are aliases of it (`yandexdisk`).
            .filter_map(|l| l.strip_prefix("        \""))
            .filter(|l| l.contains("=>"))
            .filter_map(|l| l.split('"').next())
            // Recognized to be refused with the reason (FileLu's key).
            .filter(|t| *t != "filelu")
            .collect();
        read.sort_unstable();
        read.dedup();
        let mut listed: Vec<&str> = RCLONE_BRIDGE_BACKENDS
            .iter()
            .map(|b| b.rclone_type)
            .collect();
        listed.sort_unstable();
        assert_eq!(
            read, listed,
            "map_remote and RCLONE_BRIDGE_BACKENDS disagree"
        );
        for backend in RCLONE_BRIDGE_BACKENDS {
            let conf = format!("[x]\ntype = {}\n", backend.rclone_type);
            let remotes = parse_rclone_conf(&conf);
            if let Err(e) = map_remote("x", &remotes["x"], None) {
                assert!(
                    !e.contains("unsupported rclone type"),
                    "{}: {e}",
                    backend.rclone_type
                );
            }
        }
    }

    #[test]
    fn the_readme_and_the_cli_guide_quote_the_code_count() {
        let count = rclone_published_backends().count();
        let readme = include_str!("../../README.md");
        let row = readme
            .lines()
            .find(|l| l.starts_with("| **rclone** |"))
            .expect("README bridge row");
        assert!(
            row.contains(&format!("| {count} backend types")),
            "README says {row:?}, the code carries {count}"
        );
        let guide = include_str!("../../docs/CLI-GUIDE.md");
        let sentence = guide
            .lines()
            .find(|l| l.starts_with("Supports these rclone backend types:"))
            .expect("CLI guide backend sentence");
        let listed = sentence.split(". ").next().unwrap_or(sentence);
        // Whole entries, not substrings: `SFTP` alone must not pass for `FTP`.
        let list = listed.split_once(": ").map_or(listed, |(_, l)| l);
        let list = list.split(", plus ").next().unwrap_or(list);
        // `FTP (and FTPS)` is the entry for FTP.
        let mut entries: Vec<&str> = list
            .split(", ")
            .map(|e| e.split(" (").next().unwrap_or(e).trim())
            .collect();
        if let Some(tail) = entries.pop() {
            match tail.rsplit_once(" and ") {
                Some((before, last)) => entries.extend([before.trim(), last.trim()]),
                None => entries.push(tail),
            }
        }
        for backend in rclone_published_backends() {
            let label = backend.label.unwrap();
            assert!(
                entries.contains(&label),
                "CLI guide misses {label}: {entries:?}"
            );
        }
    }

    /// Import a config written to a temp file, removing it afterwards.
    fn import_text(conf: &str, tag: &str) -> RcloneImportResult {
        let path = tmp_write(
            conf,
            &format!("aeroftp-test-{tag}-{}.conf", std::process::id()),
        );
        let result = import_rclone(&path).expect("import");
        std::fs::remove_file(&path).ok();
        result
    }

    fn imported<'a>(result: &'a RcloneImportResult, name: &str) -> &'a ServerProfileExport {
        result
            .servers
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no profile '{name}': {:?}", result.skipped))
    }

    fn skip_reason<'a>(result: &'a RcloneImportResult, name: &str) -> &'a str {
        &result
            .skipped
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("'{name}' was not skipped"))
            .reason
    }

    fn verify_cert_of(profile: &ServerProfileExport) -> Option<bool> {
        profile
            .options
            .as_ref()
            .and_then(|o| o.get("verifyCert"))
            .and_then(|v| v.as_bool())
    }

    #[test]
    fn a_boolean_is_read_in_every_spelling_rclone_takes() {
        for spelling in ["1", "t", "T", "TRUE", "true", "True"] {
            assert_eq!(rclone_bool(spelling), Some(true), "{spelling}");
        }
        for spelling in ["0", "f", "F", "FALSE", "false", "False"] {
            assert_eq!(rclone_bool(spelling), Some(false), "{spelling}");
        }
        for spelling in ["yes", "on", "tRuE", ""] {
            assert_eq!(rclone_bool(spelling), None, "{spelling}");
        }
        let result = import_text(
            "[ftps]\ntype = ftp\nhost = h.example.test\nuser = u\nexplicit_tls = True\n\
             no_check_certificate = TRUE\n",
            "bool-spellings",
        );
        assert_eq!(verify_cert_of(imported(&result, "ftps")), Some(false));
    }

    #[test]
    fn a_bare_certificate_key_turns_checks_off_only_where_rclone_reads_it() {
        let result = import_text(
            "[dav]\ntype = webdav\nurl = https://dav.example.test/\nno_check_certificate = true\n\n\
             [dav-off]\ntype = webdav\nurl = https://dav.example.test/\n\
             override.no_check_certificate = true\n\n\
             [ftp]\ntype = ftp\nhost = h.example.test\nexplicit_tls = true\nno_check_certificate = true\n",
            "bare-cert-key",
        );
        assert_eq!(
            verify_cert_of(imported(&result, "dav")),
            None,
            "rclone verifies this remote, so the profile does too"
        );
        assert_eq!(verify_cert_of(imported(&result, "dav-off")), Some(false));
        assert_eq!(verify_cert_of(imported(&result, "ftp")), Some(false));
    }

    #[test]
    fn a_key_file_path_is_expanded_as_rclone_expands_it() {
        let dir = Path::new("/etc/rclone");
        let path = std::env::var("PATH").unwrap_or_default();
        assert_eq!(
            expand_rclone_path("${RCLONE_CONFIG_DIR}/id_ed25519", Some(dir)).as_deref(),
            Some("/etc/rclone/id_ed25519")
        );
        assert_eq!(
            expand_rclone_path("$PATH/k", None),
            Some(format!("{path}/k"))
        );
        assert_eq!(
            expand_rclone_path("${PATH}-k", None),
            Some(format!("{path}-k"))
        );
        assert_eq!(
            expand_rclone_path("/k/${AEROFTP_TEST_NEVER_SET_VAR}/x", None).as_deref(),
            Some("/k//x"),
            "an unset variable is empty, as in Go's os.ExpandEnv"
        );
        assert_eq!(
            expand_rclone_path("~/.ssh/id $ x", None).as_deref(),
            Some("~/.ssh/id $ x")
        );
        assert_eq!(
            expand_rclone_path("/k/${open", None).as_deref(),
            Some("/k/${open")
        );
        // A key path past PATH_MAX stops at the cap instead of allocating:
        // `${RCLONE_CONFIG_DIR}` a thousand times over a 12-byte folder.
        let long = "${RCLONE_CONFIG_DIR}".repeat(1000);
        assert_eq!(expand_rclone_path(&long, Some(dir)), None);
        // A bare file name, as the CLI passes `rclone.conf`: the folder is the
        // current one, not the empty parent that made `/id_ed25519`.
        assert_eq!(
            rclone_config_dir(Path::new("rclone.conf")),
            Some(std::env::current_dir().unwrap())
        );

        let result = import_text(
            "[key]\ntype = sftp\nhost = h.example.test\nuser = u\n\
             key_file = ${RCLONE_CONFIG_DIR}/id_ed25519\n",
            "key-file-expand",
        );
        let key = imported(&result, "key")
            .options
            .as_ref()
            .and_then(|o| o.get("private_key_path"))
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();
        assert_eq!(
            Path::new(&key),
            std::env::temp_dir().join("id_ed25519"),
            "the folder of the file read"
        );
    }

    #[test]
    fn an_alias_never_repeats_its_connection_string() {
        let result = import_text(
            "[mys3]\ntype = s3\nprovider = Minio\nendpoint = https://minio.example.test\n\n\
             [named]\ntype = alias\nremote = mys3,secret_access_key=ALIAS_SENTINEL_1:bucket\n\n\
             [fly]\ntype = alias\nremote = :s3,secret_access_key=ALIAS_SENTINEL_2:bucket\n\n\
             [odd]\ntype = alias\nremote = \"ALIAS_SENTINEL_3\":bucket\n",
            "alias-connection-string",
        );
        for name in ["named", "fly", "odd"] {
            let reason = skip_reason(&result, name);
            assert!(!reason.contains("SENTINEL"), "{name}: {reason}");
        }
        assert!(skip_reason(&result, "named").contains("'mys3'"));
    }

    #[test]
    fn an_aeroftp_export_does_not_fold_a_folder_into_a_rooted_drive() {
        // The fold runs before `map_alias_remote`'s guard: an alias that looks
        // generated (`gd-path`) over a Drive rooted at a folder id replaced the
        // root id with `/Team`, which opened at the account root.
        let result = import_text(
            "# Generated by AeroFTP - https://aeroftp.app\n# Exported: 2026-11-06T17:00:00Z\n\n\
             [gd]\ntype = drive\nroot_folder_id = 1AbCdEf\n\n\
             [gd-path]\ntype = alias\nremote = gd:Team\n\n\
             [plain]\ntype = drive\n\n\
             [plain-path]\ntype = alias\nremote = plain:Team\n",
            "stamped-rooted-drive",
        );
        assert_eq!(
            imported(&result, "gd").initial_path.as_deref(),
            Some("1AbCdEf")
        );
        assert!(skip_reason(&result, "gd-path").contains("root_folder_id"));
        // An unrooted Drive still folds its start folder back into the profile.
        assert!(result.servers.iter().all(|s| s.name != "plain-path"));
        assert!(imported(&result, "plain")
            .initial_path
            .as_deref()
            .is_some_and(|p| p.trim_matches('/') == "Team"));
    }

    #[test]
    fn a_remote_that_would_open_another_drive_is_refused() {
        let result = import_text(
            "[sp]\ntype = onedrive\ndrive_id = b!library\ndrive_type = documentLibrary\n\n\
             [mine]\ntype = onedrive\ndrive_id = 0123abcd\ndrive_type = personal\n\n\
             [rooted]\ntype = drive\nroot_folder_id = 1AbCdEf\n\n\
             [rooted-team]\ntype = alias\nremote = rooted:Team\n\n\
             [gd]\ntype = drive\n\n\
             [gd-team]\ntype = alias\nremote = gd:Team\n\n\
             [blank]\ntype = drive\nroot_folder_id =\n\n\
             [blank-team]\ntype = alias\nremote = blank:Team\n",
            "other-drive",
        );
        assert!(skip_reason(&result, "sp").contains("SharePoint"));
        assert!(imported(&result, "mine").options.is_some());
        assert!(skip_reason(&result, "rooted-team").contains("root_folder_id"));
        assert_eq!(
            imported(&result, "gd-team").initial_path.as_deref(),
            Some("/Team")
        );
        // A blank `root_folder_id` is the account root, not a folder id.
        assert_eq!(imported(&result, "blank").initial_path, None);
        assert_eq!(
            imported(&result, "blank-team").initial_path.as_deref(),
            Some("/Team")
        );
    }

    #[test]
    fn a_relative_sftp_folder_is_under_the_login_folder_both_ways() {
        let result = import_text(
            "[nas]\ntype = sftp\nhost = nas.example.test\nuser = u\n\n\
             [nas-projects]\ntype = alias\nremote = nas:projects\n",
            "sftp-relative-alias",
        );
        assert_eq!(
            imported(&result, "nas-projects").initial_path.as_deref(),
            Some("~/projects"),
            "AeroFTP reads a start folder without `/` from the root"
        );
        let servers = vec![
            server_with(
                "proj",
                "sftp",
                "nas.example.test",
                22,
                None,
                serde_json::json!({ "initial_path": "~/projects" }),
            ),
            server_with(
                "home",
                "sftp",
                "nas.example.test",
                22,
                None,
                serde_json::json!({ "initial_path": "~" }),
            ),
        ];
        let (_, conf) = export_to_string(&servers, "sftp-relative-export");
        assert!(
            section_of(&conf, "proj-path").contains("remote = proj:projects\n"),
            "{conf}"
        );
        assert!(!conf.contains("[home-path]"), "{conf}");
    }

    #[test]
    fn a_webdav_url_keeps_its_scheme_port_and_encoded_collection() {
        let result = import_text(
            "[dav]\ntype = webdav\nurl = http://nas.example.test:8080/dav\n",
            "webdav-scheme",
        );
        let dav = imported(&result, "dav");
        assert_eq!(
            dav.options.as_ref().and_then(|o| o.get("webdavScheme")),
            Some(&serde_json::json!("http"))
        );
        let servers = vec![server_with(
            "tls80",
            "webdav",
            "dav.example.test",
            80,
            None,
            serde_json::json!({ "webdavScheme": "https" }),
        )];
        let (_, conf) = export_to_string(&servers, "webdav-port");
        assert!(
            section_of(&conf, "tls80").contains("url = https://dav.example.test:80"),
            "{conf}"
        );
        assert_eq!(
            webdav_alias_path(
                "https://dav.example.test/team%20docs/",
                "/team docs/Photos",
                "u"
            ),
            "Photos"
        );
    }

    #[test]
    fn a_swift_export_signs_in_where_the_profile_does() {
        let servers = vec![
            server_with(
                "sw",
                "swift",
                "https://auth.example.test/v3",
                443,
                None,
                serde_json::json!({ "endpoint": "https://elsewhere.example.test/v3" }),
            ),
            server_with(
                "lead",
                "swift",
                "https://auth.example.test/v3",
                443,
                None,
                serde_json::json!({ "bucket": " media" }),
            ),
            server_with(
                "trail",
                "swift",
                "https://auth.example.test/v3",
                443,
                None,
                serde_json::json!({ "bucket": "media " }),
            ),
        ];
        let (_, conf) = export_to_string(&servers, "swift-auth-host");
        let sw = section_of(&conf, "sw");
        assert!(
            sw.contains("auth = https://auth.example.test/v3\n"),
            "{conf}"
        );
        assert!(!conf.contains("elsewhere"), "{conf}");
        assert!(conf.contains("remote = lead: media\n"), "{conf}");
        assert!(!conf.contains("remote = trail:"), "{conf}");
        assert!(conf.contains("cannot hold 'media '"), "{conf}");
    }

    #[test]
    fn a_swift_export_without_a_scheme_signs_in_at_the_endpoint_the_profile_dials() {
        // profile_connection_extra dials an http(s) endpoint when the host has
        // no scheme; the export wrote `https://<host>` and sent the password
        // to a service the profile never signs in to.
        let servers = vec![
            server_with(
                "bare",
                "swift",
                "auth.example.test",
                443,
                None,
                serde_json::json!({ "endpoint": "https://keystone.example.test/v3" }),
            ),
            server_with(
                "plain",
                "swift",
                "auth.example.test/v3",
                443,
                None,
                serde_json::json!({ "endpoint": "keystone.example.test/v3" }),
            ),
        ];
        let (_, conf) = export_to_string(&servers, "swift-bare-host");
        assert!(
            section_of(&conf, "bare").contains("auth = https://keystone.example.test/v3\n"),
            "{conf}"
        );
        // An endpoint without a scheme is not dialled: the host is.
        assert!(
            section_of(&conf, "plain").contains("auth = https://auth.example.test/v3\n"),
            "{conf}"
        );
    }

    #[test]
    fn a_space_after_the_colon_is_part_of_the_folder_rclone_opens() {
        // rclone v1.75.2 opens ` sp` for `remote = loc: sp` and drops only the
        // trailing spaces, with the rest of the value.
        let bucket_of = |result: &RcloneImportResult, name: &str| {
            imported(result, name)
                .options
                .as_ref()
                .and_then(|o| o.get("bucket"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let servers = vec![server_with(
            "lead",
            "swift",
            "https://auth.example.test/v3",
            443,
            None,
            serde_json::json!({ "bucket": " media" }),
        )];
        let (_, conf) = export_to_string(&servers, "lead-space-out");
        let back = import_text(&conf, "lead-space-back");
        assert_eq!(
            bucket_of(&back, "lead").as_deref(),
            Some(" media"),
            "{conf}"
        );
        let foreign = import_text(
            "[s3r]\ntype = s3\nprovider = AWS\n\n\
             [pics]\ntype = alias\nremote = s3r: media/2026\n\n\
             [nas]\ntype = sftp\nhost = nas.example.test\nuser = u\n\n\
             [docs]\ntype = alias\nremote = nas: docs\n\n\
             [deep]\ntype = alias\nremote = docs: sub\n\n\
             [relcrypt]\ntype = crypt\nremote = nas: docs\n\
             password = Z5gL8_HnB9SyJT5RjJtYcEBwiBrSN8h0fHelLigK\n\n\
             [abscrypt]\ntype = crypt\nremote = nas:/docs\n\
             password = Z5gL8_HnB9SyJT5RjJtYcEBwiBrSN8h0fHelLigK\n",
            "lead-space-foreign",
        );
        // A crypt over SFTP opens where an alias does: under the login folder
        // for `nas: docs`, from the root for `nas:/docs`.
        assert_eq!(
            imported(&foreign, "relcrypt").initial_path.as_deref(),
            Some("~/ docs")
        );
        assert_eq!(
            imported(&foreign, "abscrypt").initial_path.as_deref(),
            Some("/docs")
        );
        assert_eq!(bucket_of(&foreign, "pics").as_deref(), Some(" media"));
        assert_eq!(
            imported(&foreign, "pics").initial_path.as_deref(),
            Some("/2026")
        );
        assert_eq!(
            imported(&foreign, "docs").initial_path.as_deref(),
            Some("~/ docs")
        );
        assert_eq!(
            imported(&foreign, "deep").initial_path.as_deref(),
            Some("~/ docs/ sub"),
            "through an alias chain too"
        );
    }

    #[test]
    fn a_hand_written_alias_in_an_aeroftp_file_stays_its_own_profile() {
        let servers = vec![server_with(
            "nas",
            "sftp",
            "nas.example.test",
            22,
            None,
            serde_json::json!({ "initial_path": "/team" }),
        )];
        let (_, conf) = export_to_string(&servers, "fold-provenance");
        let conf = format!("{conf}\n[nas-backup]\ntype = alias\nremote = nas:/backup\n");
        let result = import_text(&conf, "fold-provenance-import");
        assert_eq!(
            imported(&result, "nas").initial_path.as_deref(),
            Some("/team")
        );
        assert_eq!(
            imported(&result, "nas-backup").initial_path.as_deref(),
            Some("/backup")
        );
        assert!(is_generated_alias_name("nas-path", "nas", "sftp", "/team"));
        assert!(is_generated_alias_name(
            "nas-path-2",
            "nas",
            "sftp",
            "/team"
        ));
        assert!(!is_generated_alias_name(
            "nas-backup",
            "nas",
            "sftp",
            "backup"
        ));
        assert!(is_generated_alias_name("s3-media", "s3", "s3", "media"));
        assert!(!is_generated_alias_name(
            "s3-media",
            "s3",
            "s3",
            "media/2026"
        ));
    }

    #[test]
    fn the_export_counts_profiles_apart_from_the_remotes_they_write() {
        let servers = vec![export_server(
            "minio",
            "s3",
            Some(serde_json::json!({
                "bucket": "media",
                "endpoint": "https://minio.example.test",
                "rcloneCryptEnabled": true,
                "rcloneCryptPassword": "pw",
                "rcloneCryptOverlayScope": "/media/vault",
            })),
        )];
        let (outcome, conf) = export_to_string(&servers, "count-and-bucket-scope");
        assert_eq!(outcome.profiles, 1, "{conf}");
        assert_eq!(outcome.exported, 3, "base, bucket alias and crypt:\n{conf}");
        assert_eq!(
            crate::bridge_shared::ExportReport::into_export_parts(outcome).0,
            1,
            "the GUI and the CLI report profiles"
        );
        // The scope is under the bucket the alias opens, folder `media` included.
        assert!(
            conf.contains("remote = minio-media:/media/vault\n"),
            "{conf}"
        );
    }
}
