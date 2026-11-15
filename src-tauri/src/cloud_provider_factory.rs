// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroCloud Multi-Protocol Provider Factory
// Creates and connects a StorageProvider based on CloudConfig protocol_type

use crate::cloud_config::CloudConfig;
use crate::credential_store;
use crate::providers::{
    box_provider::BoxProvider,
    dropbox::{DropboxConfig, DropboxProvider},
    fourshared::FourSharedProvider,
    google_drive::{GoogleDriveConfig, GoogleDriveProvider},
    onedrive::{OneDriveConfig, OneDriveProvider},
    pcloud::PCloudProvider,
    types::{BoxConfig, FourSharedConfig, PCloudConfig, ProviderType},
    zoho_workdrive::{ZohoWorkdriveConfig, ZohoWorkdriveProvider},
    OAuth2Manager, OAuthProvider, ProviderConfig, ProviderFactory, StorageProvider,
    YandexDiskProvider,
};
use secrecy::SecretString;
use std::collections::HashMap;
use tracing::info;

/// Parse a server field that may contain an embedded port.
///
/// Handles formats saved by CloudPanel:
/// - `"hostname"` → `("hostname", None)`
/// - `"hostname:22"` → `("hostname", Some(22))`
/// - `"hostname/webdav/path"` → `("hostname/webdav/path", None)`
/// - `"hostname/webdav/path:80"` → `("hostname/webdav/path", Some(80))` (CloudPanel bug)
/// - `"hostname:80/webdav/path"` → `("hostname/webdav/path", Some(80))` (correct format)
pub(crate) fn parse_server_field(server: &str) -> (String, Option<u16>) {
    // A3-07: Handle IPv6 addresses in bracket notation: [::1]:22, [2001:db8::1]
    if server.starts_with('[') {
        if let Some(bracket_end) = server.find(']') {
            let host = &server[1..bracket_end];
            let rest = &server[bracket_end + 1..];
            let port = if let Some(stripped) = rest.strip_prefix(':') {
                stripped
                    .split('/')
                    .next()
                    .and_then(|p| p.parse::<u16>().ok())
            } else {
                None
            };
            return (host.to_string(), port);
        }
    }

    // If server contains a path (slash), handle host:port/path vs host/path:port
    if let Some(slash_pos) = server.find('/') {
        let host_part = &server[..slash_pos];
        let path_part = &server[slash_pos..];

        // Check for port in host part: "hostname:80/path"
        if let Some(colon_pos) = host_part.rfind(':') {
            if let Ok(port) = host_part[colon_pos + 1..].parse::<u16>() {
                let hostname = &host_part[..colon_pos];
                return (format!("{}{}", hostname, path_part), Some(port));
            }
        }

        // Check for port appended to end (CloudPanel bug): "hostname/path:80"
        if let Some(colon_pos) = server.rfind(':') {
            if colon_pos > slash_pos {
                if let Ok(port) = server[colon_pos + 1..].parse::<u16>() {
                    return (server[..colon_pos].to_string(), Some(port));
                }
            }
        }

        return (server.to_string(), None);
    }

    // No path: just "hostname" or "hostname:port"
    if let Some(colon_pos) = server.rfind(':') {
        if let Ok(port) = server[colon_pos + 1..].parse::<u16>() {
            return (server[..colon_pos].to_string(), Some(port));
        }
    }

    (server.to_string(), None)
}

/// Create and connect a provider for AeroCloud background sync.
///
/// Dispatches based on `config.protocol_type`:
/// - Direct auth (FTP, SFTP, WebDAV, S3, Azure, MEGA, Filen, Internxt, kDrive, Jottacloud, ...):
///   connects the saved profile `config.server_profile` names through ProviderFactory
/// - OAuth2 (Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho) and OAuth1 (4shared):
///   the OAuth app from the vault and the tokens of that profile, or the app a
///   config written by the old setup wizard carries in connection_params
pub async fn create_cloud_provider(
    config: &CloudConfig,
) -> Result<Box<dyn StorageProvider>, String> {
    Ok(create_cloud_connection(config).await?.provider)
}

/// The provider and the identity of the exact connection settings consumed by its builder.
pub(crate) struct OpenedCloudProvider {
    pub provider: Box<dyn StorageProvider>,
    pub identity: Option<String>,
    pub overlay_binding: serde_json::Value,
}

/// Hash canonical structured identity without persisting credentials or depending on map order.
fn identity_digest(value: serde_json::Value) -> String {
    use sha2::Digest;
    fn canonical(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(values) => serde_json::Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonical).collect())
            }
            value => value,
        }
    }
    format!(
        "{:x}",
        sha2::Sha256::digest(canonical(value).to_string().as_bytes())
    )
}

/// Fingerprint the resolved endpoint, account, root and options actually passed to ProviderFactory.
fn resolved_target_identity(
    config: &ProviderConfig,
    binding: &serde_json::Value,
    account: Option<&str>,
) -> Option<String> {
    let username = config
        .username
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let account = account.filter(|value| !value.trim().is_empty());
    // These are the actual login or namespace selectors consumed by each builder,
    // not the display username (token/bridge profiles may carry a placeholder).
    let principal = match config.provider_type {
        ProviderType::Ftp
        | ProviderType::Ftps
        | ProviderType::Sftp
        | ProviderType::Scp
        | ProviderType::WebDav
        | ProviderType::S3
        | ProviderType::Azure
        | ProviderType::Mega
        | ProviderType::Filen
        | ProviderType::Backblaze => username,
        ProviderType::Uploadcare => config
            .extra
            .get("public_key")
            .map(String::as_str)
            .or(username),
        ProviderType::KDrive => config.extra.get("drive_id").map(String::as_str),
        _ => None,
    }
    .filter(|value| !value.trim().is_empty());
    if account.is_none() && principal.is_none() {
        return None;
    }
    let options: std::collections::BTreeMap<_, _> = config
        .extra
        .iter()
        // Only selectors that can change the addressed namespace belong here.
        // Retry/session/TLS behavior and one-time authentication inputs do not.
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "endpoint"
                    | "bucket"
                    | "region"
                    | "container"
                    | "account_name"
                    | "drive_id"
                    | "device"
                    | "mountpoint"
                    | "root_folder_id"
                    | "cloud_name"
                    | "project_id"
                    | "tenant_id"
                    | "role_arn"
                    | "public_key"
                    | "space_id"
                    | "library_id"
            )
        })
        .collect();
    Some(identity_digest(serde_json::json!({
        "protocol": config.provider_type.to_string(), "host": config.host, "port": config.port.unwrap_or(config.provider_type.default_port()),
        "principal": principal, "account": account, "root": config.initial_path,
        "options": options, "overlay": overlay_identity(binding),
    })))
}

/// Canonical mapping options, excluding UI-only fields and explicit defaults.
fn overlay_identity(binding: &serde_json::Value) -> serde_json::Value {
    if binding.get("enabled").and_then(serde_json::Value::as_bool) != Some(true) {
        return serde_json::Value::Null;
    }
    let mut identity = serde_json::json!({
        "enabled":true,
        "kind": binding.get("kind").and_then(serde_json::Value::as_str).unwrap_or(crate::crypt_overlay_provider::DEFAULT_OVERLAY_KIND),
        "remoteScope":binding.get("remoteScope").and_then(serde_json::Value::as_str).unwrap_or(""),
        "filenameEncryption":binding.get("filenameEncryption").and_then(serde_json::Value::as_str).unwrap_or("standard"),
        "directoryNameEncryption":binding.get("directoryNameEncryption").and_then(serde_json::Value::as_bool).unwrap_or(true),
        "withHeader":binding.get("withHeader").and_then(serde_json::Value::as_bool).unwrap_or(false),
        "useDefaultSalt":binding.get("useDefaultSalt").and_then(serde_json::Value::as_bool).unwrap_or(false),
    });
    // Only where the identity's root does not already hold the overlay's
    // folder (`identity_overlay_binding`): every other identity stays as it was.
    if let Some(anchor) = binding
        .get(IDENTITY_ANCHOR_KEY)
        .and_then(serde_json::Value::as_str)
    {
        identity["anchor"] = serde_json::Value::String(anchor.to_string());
    }
    identity
}

fn overlay_binding(profile: Option<&serde_json::Value>) -> serde_json::Value {
    profile
        .and_then(|profile| profile.get("aeroCryptOverlay"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// Key of the overlay's anchor in the binding the identity is built from
/// (never in the binding the overlay is built with).
const IDENTITY_ANCHOR_KEY: &str = "identityAnchor";

/// The overlay binding as the identity reads it. With no `remoteScope` the
/// overlay sits at the starting folder as written (`profile_overlay_scope`),
/// edge spaces included, while an identity whose remote folder does not
/// depend on the root keeps the root trimmed (`identity_root`): `/reports `
/// and `/reports` were one identity for two encrypted folders. Only there,
/// the anchor joins the identity, so every profile whose two spellings agree
/// keeps its identity, and only an overlay profile with a padded root gets a
/// new one, once.
fn identity_overlay_binding(
    profile: Option<&serde_json::Value>,
    remote_folder: &str,
    binding: &serde_json::Value,
) -> serde_json::Value {
    let mut binding = binding.clone();
    let Some(profile) = profile else {
        return binding;
    };
    let enabled = binding.get("enabled").and_then(serde_json::Value::as_bool) == Some(true);
    let scoped = binding
        .get("remoteScope")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|scope| !scope.is_empty());
    if enabled && !scoped && remote_folder_is_root_independent(remote_folder, profile) {
        let anchor = crate::crypt_overlay_provider::profile_overlay_scope(profile);
        if anchor.trim() != anchor {
            binding[IDENTITY_ANCHOR_KEY] = serde_json::Value::String(anchor);
        }
    }
    binding
}

/// Recheck overlay selection around composition so a profile edit cannot mix a new binding with an old connection.
pub(crate) fn current_overlay_binding(
    config: &CloudConfig,
    store: &credential_store::CredentialStore,
) -> Result<serde_json::Value, String> {
    Ok(overlay_binding(saved_profile(store, config)?.as_ref()))
}

pub(crate) async fn create_cloud_connection(
    config: &CloudConfig,
) -> Result<OpenedCloudProvider, String> {
    match config.protocol_type.as_str() {
        // --- Direct auth providers: use ProviderFactory ---
        "ftp" => create_via_factory(config, ProviderType::Ftp).await,
        "ftps" => create_via_factory(config, ProviderType::Ftps).await,
        "sftp" => create_via_factory(config, ProviderType::Sftp).await,
        "scp" => create_via_factory(config, ProviderType::Scp).await,
        "webdav" => create_via_factory(config, ProviderType::WebDav).await,
        "s3" => create_via_factory(config, ProviderType::S3).await,
        "azure" => create_via_factory(config, ProviderType::Azure).await,
        "mega" => create_via_factory(config, ProviderType::Mega).await,
        "proton" | "protondrive" => create_via_factory(config, ProviderType::Proton).await,
        "filen" => create_via_factory(config, ProviderType::Filen).await,
        "internxt" => create_via_factory(config, ProviderType::Internxt).await,
        "kdrive" => create_via_factory(config, ProviderType::KDrive).await,
        "jottacloud" => create_via_factory(config, ProviderType::Jottacloud).await,
        "filelu" => create_via_factory(config, ProviderType::FileLu).await,
        "koofr" => create_via_factory(config, ProviderType::Koofr).await,
        "twake" => create_via_factory(config, ProviderType::Twake).await,
        "opendrive" => create_via_factory(config, ProviderType::OpenDrive).await,
        "yandexdisk" => create_yandex(config).await,
        "imagekit" => create_via_factory(config, ProviderType::ImageKit).await,
        "uploadcare" => create_via_factory(config, ProviderType::Uploadcare).await,
        "cloudinary" => create_via_factory(config, ProviderType::Cloudinary).await,
        "backblaze" | "b2" | "backblazeb2" => {
            create_via_factory(config, ProviderType::Backblaze).await
        }

        // --- OAuth2 providers: direct instantiation ---
        "googledrive" => create_google_drive(config).await,
        "dropbox" => create_dropbox(config).await,
        "onedrive" => create_onedrive(config).await,
        "box" => create_box(config).await,
        "pcloud" => create_pcloud(config).await,
        "zohoworkdrive" => create_zoho(config).await,

        // --- OAuth1 provider ---
        "fourshared" => create_fourshared(config).await,

        other => Err(format!("Unsupported protocol for AeroCloud: {}", other)),
    }
}

// ── Direct auth providers ──────────────────────────────────────────

/// Connection credentials for a StorageProvider as AeroCloud stores them.
#[derive(serde::Deserialize)]
struct SavedCreds {
    #[serde(default)]
    server: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

/// The saved profile an AeroCloud config names (by id, or by exact name for a
/// legacy config): the selection the crypt overlay stack makes, so the
/// connection and the encryption binding of a sync always come from the same
/// profile.
fn saved_profile(
    store: &credential_store::CredentialStore,
    config: &CloudConfig,
) -> Result<Option<serde_json::Value>, String> {
    let has_id = config
        .server_profile_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty());
    if !has_id && config.server_profile.is_empty() {
        return Ok(None);
    }
    let profiles = crate::user_partitions::mcp_list_active_server_profiles(store)?;
    Ok(crate::crypt_overlay_provider::select_aerocloud_profile(
        &profiles,
        config.server_profile_id.as_deref(),
        &config.server_profile,
    )
    .cloned())
}

/// The folder a saved profile opens in, which is the root AeroCloud works in:
/// the profile's starting folder with the `{username}` template some WebDAV
/// presets keep in it resolved, `/` when it has none. AeroCloud connects
/// there, as every other surface opens the profile, so a crypt overlay bound
/// to the profile anchors where it always does, and the sync creates the
/// remote folder (which may not exist yet) inside it. On SFTP and SCP a
/// relative folder is under the login folder (`~/docs`), where the connect
/// opens it: entered again as `docs`, the root became `docs/docs`. The root
/// is also in the target identity, so a config made when `docs` read from
/// the root starts a fresh index instead of reading the old folder's state
/// against another folder.
pub(crate) fn profile_root(profile: &serde_json::Value) -> Result<String, String> {
    // The folder is used as written (#1449), never trimmed before or after
    // the fill, as the connect opens it and the overlay anchor reads it
    // (`profile_overlay_scope`): a trimmed root sat outside its own encrypted
    // folder. Only a blank folder means the server root.
    let folder = filled_starting_folder(profile)?;
    Ok(placed_root(&folder, profile))
}

/// The root an AeroCloud identity carries. The identity follows the data
/// path. The sync enters the root, then `cd`s into the config's
/// `remote_folder`. When that folder does not depend on the root (absolute,
/// or under the login folder on SFTP and SCP), the data path never moved with
/// the root, so the identity keeps the form every existing index was made with
/// (filled, trimmed, placed, as `profile_root` read it before it took the
/// folder as written): a folder with edge spaces does not re-index. When the
/// folder is opened under the root (relative or blank), the data path is the
/// literal root plus that folder, so the identity carries the literal root:
/// a root that changes by one space is another folder, and a baseline made for
/// one must never plan against the other. Pinned by
/// `identity_root_follows_the_data_path`.
fn identity_root(profile: &serde_json::Value, remote_folder: &str) -> Result<String, String> {
    let folder = filled_starting_folder(profile)?;
    Ok(
        if remote_folder_is_root_independent(remote_folder, profile) {
            placed_root(folder.trim(), profile)
        } else {
            placed_root(&folder, profile)
        },
    )
}

/// Whether `remote_folder` names a folder the root does not place, as `cd`
/// resolves it after the root is entered: an absolute path, or on SFTP and
/// SCP one under the login folder (`~`, `~/...`,
/// [`crate::providers::sftp::resolve_remote_path`]). A blank folder is the
/// root itself and any other relative one is opened under it.
fn remote_folder_is_root_independent(remote_folder: &str, profile: &serde_json::Value) -> bool {
    let protocol = profile
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if remote_folder == "/" && SLASH_IS_THE_STARTING_FOLDER.contains(&protocol) {
        return false;
    }
    remote_folder.starts_with('/')
        || (is_ssh_profile(profile)
            && crate::crypt_overlay_provider::is_login_relative_scope(remote_folder))
}

/// The protocols whose `/` is the profile's starting folder rather than the
/// top of the server, container or bucket. On S3 the starting folder is the
/// bucket prefix (`S3Config::prefix` is `initial_path`) and `/` reads that
/// prefix (`S3Provider::prefix_of`). On WebDAV the connect keeps the starting
/// folder, as written, as its server root (`WebDavProvider::connect`), and
/// `cd("/")` and `list("/")` both go there. On both, a remote folder of `/`
/// follows the root. Azure Blob reads `/` as the container root
/// (`AzureProvider::cd`), Swift as the container root (`SwiftProvider::cd`
/// joins an absolute path as given), B2 as the bucket root (`b2_key` of the
/// absolute path), Cloudinary and ImageKit as their top (`resolve_path`
/// keeps an absolute path), FTP, FTPS, SFTP and SCP as the server's `/`, and
/// every other provider as the account root (its connect only enters the
/// starting folder). Pinned by
/// `a_slash_remote_folder_follows_the_root_only_where_slash_is_the_starting_folder`
/// and, on the wire, `a_webdav_slash_lists_the_root_the_identity_names`.
const SLASH_IS_THE_STARTING_FOLDER: &[&str] = &["s3", "webdav"];

/// What is left to do with the profile root once the provider has connected,
/// given where it stands (`pwd`).
#[derive(Debug, PartialEq, Eq)]
enum RootStep {
    /// A blank root or `/`: nothing to enter.
    Nothing,
    /// Enter the root as written. An absolute root is always entered:
    /// entering it again changes nothing, and it is the check that the folder
    /// exists, which SFTP's connect does not make (it sets the folder it
    /// reports without asking the server). A relative root is entered when the
    /// provider has not placed it.
    Enter,
    /// A relative root the provider already entered (FTP, SFTP): the folder it
    /// reports is confirmed by its absolute path, so a missing root still
    /// fails the connect, and the root is not stepped into twice
    /// (`/home/u/ /reports/ /reports`).
    Confirm,
}

/// Decided from the path itself: a relative root counts as placed when the
/// folder the provider reports ends with the root's own segments (after a
/// leading `~/` or `./`). A provider that ignores `initial_path` stands in its
/// top or login folder (Filen at `/`, a login folder such as `/home/u`), and
/// the root is entered there.
fn root_step(root: &str, here: &str) -> RootStep {
    if root.trim().is_empty() || root == "/" {
        return RootStep::Nothing;
    }
    if root.starts_with('/') {
        return RootStep::Enter;
    }
    let segments = root
        .strip_prefix("~/")
        .or_else(|| root.strip_prefix("./"))
        .unwrap_or(root)
        .trim_end_matches('/');
    let here = here.trim_end_matches('/');
    let placed =
        !segments.is_empty() && (here == segments || here.ends_with(&format!("/{segments}")));
    if placed {
        RootStep::Confirm
    } else {
        RootStep::Enter
    }
}

/// Enter the profile root after the connect, once (`root_step`).
async fn enter_root_once(provider: &mut dyn StorageProvider, root: &str) -> Result<(), String> {
    let here = provider.pwd().await.unwrap_or_default();
    match root_step(root, &here) {
        RootStep::Nothing => Ok(()),
        RootStep::Enter => enter_profile_root(provider, Some(root)).await,
        RootStep::Confirm => provider
            .cd(&here)
            .await
            .map_err(|e| format!("Cannot open the profile folder '{}': {}", root, e)),
    }
}

fn is_ssh_profile(profile: &serde_json::Value) -> bool {
    matches!(
        profile.get("protocol").and_then(|v| v.as_str()),
        Some("sftp" | "scp")
    )
}

/// The starting folder with `{username}` filled. A user name that cannot fill
/// the template keeps it as written, never a shorter path, and a folder still
/// holding the placeholder is refused: synced as is, it would be a server
/// folder literally named `{username}`.
fn filled_starting_folder(profile: &serde_json::Value) -> Result<String, String> {
    let username = profile
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let raw = profile
        .get("initialPath")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let folder = crate::connection_manifest::expand_username_in_path(raw, username)
        .unwrap_or_else(|_| raw.to_string());
    crate::connection_manifest::refuse_unresolved_username_template(&folder)?;
    Ok(folder)
}

/// A filled folder as the protocol opens it: SFTP and SCP place a relative one
/// under the login folder; a blank one is the server root.
fn placed_root(folder: &str, profile: &serde_json::Value) -> String {
    let ssh = is_ssh_profile(profile);
    if folder.trim().is_empty() {
        "/".to_string()
    } else {
        crate::crypt_overlay_provider::login_relative_form(folder, ssh).into_owned()
    }
}

/// Open the profile root on a provider that connected at the account root
/// (the OAuth providers take no starting folder). A legacy config without a
/// saved profile has no root to open.
async fn enter_profile_root(
    provider: &mut dyn StorageProvider,
    root: Option<&str>,
) -> Result<(), String> {
    match root {
        Some(root) if root != "/" => provider
            .cd(root)
            .await
            .map_err(|e| format!("Cannot open the profile folder '{}': {}", root, e)),
        _ => Ok(()),
    }
}

/// Connection credentials of a saved profile: host and username from the
/// profile record, the password from the canonical `server_{id}` credential
/// (or a legacy inline password), the way the normal connect path reads them.
fn profile_creds(
    store: &credential_store::CredentialStore,
    profile: &serde_json::Value,
) -> Result<SavedCreds, String> {
    let name = profile.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let id = profile
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Profile '{}' has no id", name))?;
    let profile_username = profile
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // The vault credential may be the bare password or a structured blob
    // (username plus password, access token or API key), read the way the MCP
    // pool reads it; a legacy inline password is the fallback.
    let (username, password) =
        match crate::user_partitions::resolve_active_credential(store, &format!("server_{}", id))
            .ok()
            .flatten()
        {
            Some(raw) => crate::profile_loader::parse_server_credential(&raw, profile_username),
            None => (
                profile_username.to_string(),
                profile
                    .get("password")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
        };
    let host = profile
        .get("server")
        .or_else(|| profile.get("host"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // Embed the profile port so parse_server_field picks it up (the host field
    // usually carries no port of its own).
    let server = match profile.get("port").and_then(|v| v.as_u64()) {
        Some(port) if !host.is_empty() && !host.contains(':') => format!("{}:{}", host, port),
        _ => host,
    };
    Ok(SavedCreds {
        server,
        username,
        password,
    })
}

/// Provider options for an AeroCloud connection and the host to dial: the
/// saved profile's own (bucket, region, endpoint, container, key path, ...,
/// through the loader the MCP pool uses too), then the config's
/// `connection_params` on top. Only configs written by the old setup wizard
/// carry params, and for those an explicit value has always won. The profile
/// id is stamped last, so no param can point the per-profile token lookup at
/// another profile.
fn connection_extra(
    profile: Option<&serde_json::Value>,
    provider_type: ProviderType,
    host: &str,
    connection_params: &serde_json::Value,
) -> (HashMap<String, String>, String) {
    let (mut extra, host) = match profile {
        Some(profile) => {
            crate::profile_loader::profile_connection_extra(profile, provider_type, host)
        }
        None => (HashMap::new(), host.to_string()),
    };
    if let Some(obj) = connection_params.as_object() {
        for (k, v) in obj {
            let value = if let Some(s) = v.as_str() {
                s.to_string()
            } else if let Some(n) = v.as_u64() {
                n.to_string()
            } else if let Some(b) = v.as_bool() {
                b.to_string()
            } else {
                continue;
            };
            extra.insert(k.clone(), value);
        }
    }
    if let Some(id) = profile
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        extra.insert("profile_id".to_string(), id.to_string());
    }
    (extra, host)
}

/// Connect a direct-auth provider for AeroCloud.
///
/// The saved profile the config names is the source of the connection: host,
/// user and password as the profile holds them now, and every provider option
/// it stores. The `server_{name}` blob the old setup wizard copied credentials
/// into is read only when no saved profile carries that name, which is the
/// case of a config set up with hand-typed credentials: preferring it would
/// keep connecting with a password changed since in the profile.
async fn create_via_factory(
    config: &CloudConfig,
    provider_type: ProviderType,
) -> Result<OpenedCloudProvider, String> {
    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Credential vault not open".to_string())?;

    let profile = saved_profile(&store, config)?;
    let mut creds = match &profile {
        Some(profile) => profile_creds(&store, profile)?,
        // MUV-3: resolve_active_credential prefers the active user's per-user
        // store and falls back to the legacy (dual-written) vault.
        None => {
            let json = crate::user_partitions::resolve_active_credential(
                &store,
                &format!("server_{}", config.server_profile),
            )
            .ok()
            .flatten()
            .ok_or_else(|| format!("No credentials for profile '{}'", config.server_profile))?;
            serde_json::from_str::<SavedCreds>(&json)
                .map_err(|e| format!("Failed to parse credentials: {}", e))?
        }
    };
    if let Some(profile) = &profile {
        crate::profile_loader::apply_local_bridge_credential_defaults(
            profile.get("providerId").and_then(|v| v.as_str()),
            &mut creds.username,
            &mut creds.password,
        );
    }

    // Parse host and embedded port from server field
    // CloudPanel may save "host:port" or "host/path:port" concatenated
    let (parsed_host, embedded_port) = parse_server_field(&creds.server);
    let (extra, host) = connection_extra(
        profile.as_ref(),
        provider_type,
        &parsed_host,
        &config.connection_params,
    );

    // Port priority: connection_params > embedded in server field > provider default
    let port = extra
        .get("port")
        .and_then(|p| p.parse::<u16>().ok())
        .or(embedded_port);

    let mut provider_config = ProviderConfig {
        name: config.cloud_name.clone(),
        provider_type,
        host: host.clone(),
        port,
        username: Some(creds.username.clone()),
        password: Some(creds.password.clone()),
        // A legacy config without a saved profile keeps starting in its
        // remote folder, as it always has.
        initial_path: Some(
            profile
                .as_ref()
                .map(profile_root)
                .transpose()?
                .unwrap_or_else(|| config.remote_folder.clone()),
        ),
        extra,
    };

    let binding = overlay_binding(profile.as_ref());
    let mut provider = ProviderFactory::create(&provider_config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;
    // A3-05: Zeroize password after it has been consumed by the provider
    provider_config.zeroize_password();
    // A-L2: the SFTP key passphrase reaches `extra` from the profile options and
    // is NOT covered by `zeroize_password`, which only clears the password
    // field. Scrub it here so the "something you have" secret does not linger in
    // this config's memory after the provider has consumed it. `private_key_path`
    // is a filesystem path, not secret material, so it is left as-is.
    if let Some(passphrase) = provider_config.extra.get_mut("key_passphrase") {
        use zeroize::Zeroize;
        passphrase.zeroize();
    }

    // The root is resolved before the connect: one that cannot be filled is
    // refused before any request.
    let root = profile.as_ref().map(profile_root).transpose()?;
    info!("AeroCloud: connecting via {:?} to {}", provider_type, host);
    provider
        .connect()
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;
    // Not every provider starts in `initial_path` (Filen opens at the account
    // root), so the root is entered explicitly, as the CLI does before it wraps
    // a crypt overlay: the overlay unlocks where the provider stands.
    if let Some(root) = &root {
        enter_root_once(&mut *provider, root).await?;
    }

    // Config identity is immutable here; authenticated identity further isolates API accounts.
    // The provider is built: the config now only feeds the identity, whose
    // root keeps the trimmed form every existing index was made with.
    if let Some(profile) = &profile {
        provider_config.initial_path = Some(identity_root(profile, &config.remote_folder)?);
    }
    let identity = resolved_target_identity(
        &provider_config,
        &identity_overlay_binding(profile.as_ref(), &config.remote_folder, &binding),
        provider
            .sync_namespace_identity()
            .or_else(|| provider.account_email())
            .as_deref(),
    );
    Ok(OpenedCloudProvider {
        provider,
        identity,
        overlay_binding: binding,
    })
}

/// Yandex Disk profiles either authorize with OAuth, the tokens living in the
/// profile's slot where the connect path and the CLI read them, or carry a
/// token typed into the password field. The OAuth tokens win when the profile
/// has them; otherwise the typed token connects through the factory.
async fn create_yandex(config: &CloudConfig) -> Result<OpenedCloudProvider, String> {
    if let Some(store) = credential_store::CredentialStore::from_cache() {
        let profile = saved_profile(&store, config)?;
        let profile_id = profile
            .as_ref()
            .and_then(|p| p.get("id").and_then(|v| v.as_str()).map(str::to_string))
            .unwrap_or_default();
        let manager = OAuth2Manager::new();
        if !profile_id.is_empty() && manager.has_tokens(OAuthProvider::YandexDisk, &profile_id) {
            let tokens = manager
                .load_tokens(OAuthProvider::YandexDisk, &profile_id)
                .map_err(|e| format!("No Yandex Disk tokens: {}", e))?;
            return open_yandex_oauth(
                YandexDiskProvider::new(tokens.access_token.clone(), None),
                &profile_id,
                profile.as_ref(),
                &config.remote_folder,
            )
            .await;
        }
    }
    create_via_factory(config, ProviderType::YandexDisk).await
}

/// The identity of a Yandex Disk AeroCloud target: the profile id, the root
/// in its identity form (`identity_root`, `None` without a profile), the account and the
/// overlay binding. Every existing config is matched by this digest, so its
/// inputs keep the exact form and serialization they had: the root is an
/// `Option<&str>`, as `profile.map(profile_root)` gave before the root could
/// be refused. Pinned by `yandex_identity_digest_is_unchanged_for_existing_configs`.
fn yandex_identity(
    profile_id: &str,
    root: Option<&str>,
    account: &str,
    binding: &serde_json::Value,
) -> String {
    identity_digest(serde_json::json!([
        "yandexdisk",
        profile_id,
        root,
        account,
        overlay_identity(binding)
    ]))
}

/// Finish both production OAuth-token connections and isolated provider probes.
async fn open_yandex_oauth(
    mut provider: YandexDiskProvider,
    profile_id: &str,
    profile: Option<&serde_json::Value>,
    remote_folder: &str,
) -> Result<OpenedCloudProvider, String> {
    let root = profile.map(profile_root).transpose()?;
    let identity_root = profile
        .map(|profile| identity_root(profile, remote_folder))
        .transpose()?;
    provider
        .connect()
        .await
        .map_err(|error| format!("Yandex Disk: {error}"))?;
    enter_profile_root(&mut provider, root.as_deref()).await?;
    let binding = overlay_binding(profile);
    let identity = provider
        .sync_namespace_identity()
        .or_else(|| provider.account_email())
        .filter(|account| !account.trim().is_empty())
        .map(|account| {
            let identity_binding = identity_overlay_binding(profile, remote_folder, &binding);
            yandex_identity(
                profile_id,
                identity_root.as_deref(),
                &account,
                &identity_binding,
            )
        });
    Ok(OpenedCloudProvider {
        provider: Box::new(provider),
        identity,
        overlay_binding: binding,
    })
}

// ── OAuth providers ────────────────────────────────────────────────

fn get_param<'a>(config: &'a CloudConfig, key: &str) -> Result<&'a str, String> {
    config
        .connection_params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing '{}' in connection_params", key))
}

/// The OAuth app an AeroCloud config connects with and the profile whose
/// tokens it uses.
struct OAuthApp {
    overlay_binding: serde_json::Value,
    client_id: String,
    client_secret: String,
    /// Empty for a legacy config: the provider's singleton token slot.
    profile_id: String,
    /// The region the profile or the config names, if any.
    region: Option<String>,
    /// The profile root to open after connecting; `None` for a legacy config.
    root: Option<String>,
}

/// Resolve the OAuth app of a config. One written by the old setup wizard
/// carries its own app in `connection_params` (`id_key` / `secret_key`) and
/// authorized into the provider's singleton token slot; any other config takes
/// both from the saved profile it names, as a normal connect does: the app from
/// the vault, the tokens from the profile's own slot (#214).
fn oauth_app(config: &CloudConfig, id_key: &str, secret_key: &str) -> Result<OAuthApp, String> {
    let param_region = config
        .connection_params
        .get("region")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let (Ok(client_id), Ok(client_secret)) =
        (get_param(config, id_key), get_param(config, secret_key))
    {
        return Ok(OAuthApp {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            profile_id: String::new(),
            region: param_region,
            root: None,
            overlay_binding: serde_json::Value::Null,
        });
    }

    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Credential vault not open".to_string())?;
    let profile = saved_profile(&store, config)?
        .ok_or_else(|| format!("No saved profile named '{}'", config.server_profile))?;
    let cred_key = crate::bridge_commands::oauth_client_cred_key(&config.protocol_type)
        .ok_or_else(|| format!("'{}' has no OAuth app", config.protocol_type))?;
    let (client_id, client_secret) =
        crate::bridge_commands::resolve_oauth_client_config(&store, cred_key);
    if client_id.is_empty() {
        return Err(format!(
            "No OAuth app credentials for '{}' in the vault: connect the profile once from My Servers",
            config.protocol_type
        ));
    }
    let mut options = HashMap::new();
    crate::profile_loader::apply_profile_options(&mut options, &profile);
    Ok(OAuthApp {
        client_id,
        client_secret,
        profile_id: options.remove("profile_id").unwrap_or_default(),
        region: options.remove("region").or(param_region),
        root: Some(profile_root(&profile)?),
        overlay_binding: overlay_binding(Some(&profile)),
    })
}

/// OAuth receipts require a stable authenticated account, not merely a reusable token slot.
fn opened_oauth(provider: Box<dyn StorageProvider>, app: &OAuthApp) -> OpenedCloudProvider {
    let account = provider
        .sync_namespace_identity()
        .or_else(|| provider.account_email())
        .filter(|value| !value.trim().is_empty());
    let endpoint = provider.endpoint_identity();
    let identity = account.map(|account| {
        identity_digest(serde_json::json!({
            "protocol": endpoint.protocol, "host": endpoint.host, "account": account,
            "app": app.client_id, "slot": app.profile_id, "region": app.region,
            "root": app.root, "overlay": overlay_identity(&app.overlay_binding),
        }))
    });
    OpenedCloudProvider {
        provider,
        identity,
        overlay_binding: app.overlay_binding.clone(),
    }
}

async fn create_google_drive(config: &CloudConfig) -> Result<OpenedCloudProvider, String> {
    let app = oauth_app(config, "client_id", "client_secret")?;
    let gc = GoogleDriveConfig::new(&app.client_id, &app.client_secret);
    let mut p = GoogleDriveProvider::new(gc).with_profile_id(&app.profile_id);
    p.connect()
        .await
        .map_err(|e| format!("Google Drive: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to Google Drive");
    Ok(opened_oauth(Box::new(p), &app))
}

async fn create_dropbox(config: &CloudConfig) -> Result<OpenedCloudProvider, String> {
    let app = oauth_app(config, "client_id", "client_secret")?;
    let dc = DropboxConfig::new(&app.client_id, &app.client_secret);
    let mut p = DropboxProvider::new(dc).with_profile_id(&app.profile_id);
    p.connect().await.map_err(|e| format!("Dropbox: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to Dropbox");
    Ok(opened_oauth(Box::new(p), &app))
}

async fn create_onedrive(config: &CloudConfig) -> Result<OpenedCloudProvider, String> {
    let app = oauth_app(config, "client_id", "client_secret")?;
    let oc = OneDriveConfig::new(&app.client_id, &app.client_secret);
    let mut p = OneDriveProvider::new(oc).with_profile_id(&app.profile_id);
    p.connect().await.map_err(|e| format!("OneDrive: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to OneDrive");
    Ok(opened_oauth(Box::new(p), &app))
}

async fn create_box(config: &CloudConfig) -> Result<OpenedCloudProvider, String> {
    let app = oauth_app(config, "client_id", "client_secret")?;
    let bc = BoxConfig::new(&app.client_id, &app.client_secret);
    let mut p = BoxProvider::new(bc).with_profile_id(&app.profile_id);
    p.connect().await.map_err(|e| format!("Box: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to Box");
    Ok(opened_oauth(Box::new(p), &app))
}

async fn create_pcloud(config: &CloudConfig) -> Result<OpenedCloudProvider, String> {
    let mut app = oauth_app(config, "client_id", "client_secret")?;
    // pCloud tokens are region-locked: the region detected during the token
    // exchange (vault) wins, as in the normal connect path.
    let region = credential_store::CredentialStore::from_cache()
        .and_then(|store| store.get("oauth_pcloud_region").ok())
        .or(app.region.clone())
        .unwrap_or_else(|| "us".to_string());
    app.region = Some(region.clone());
    let pc = PCloudConfig::new(&app.client_id, &app.client_secret, &region);
    let mut p = PCloudProvider::new(pc).with_profile_id(&app.profile_id);
    p.connect().await.map_err(|e| format!("pCloud: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to pCloud ({})", region);
    Ok(opened_oauth(Box::new(p), &app))
}

async fn create_zoho(config: &CloudConfig) -> Result<OpenedCloudProvider, String> {
    let mut app = oauth_app(config, "client_id", "client_secret")?;
    // The profile's region, then the one stored at authorization, as the normal
    // connect path resolves it.
    let region = app
        .region
        .clone()
        .or_else(|| {
            credential_store::CredentialStore::from_cache()
                .and_then(|store| store.get("oauth_zohoworkdrive_region").ok())
        })
        .unwrap_or_else(|| "us".to_string());
    app.region = Some(region.clone());
    let zc = ZohoWorkdriveConfig::new(&app.client_id, &app.client_secret, &region);
    let mut p = ZohoWorkdriveProvider::new(zc).with_profile_id(&app.profile_id);
    p.connect()
        .await
        .map_err(|e| format!("Zoho WorkDrive: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to Zoho WorkDrive ({})", region);
    Ok(opened_oauth(Box::new(p), &app))
}

// ── OAuth1 (4shared) ───────────────────────────────────────────────

async fn create_fourshared(config: &CloudConfig) -> Result<OpenedCloudProvider, String> {
    let app = oauth_app(config, "consumer_key", "consumer_secret")?;
    // The tokens live where the 4shared connect path stores them: the
    // profile's slot, or the provider singleton for a legacy config.
    let (access_token, access_token_secret) =
        crate::provider_commands::load_fourshared_tokens(&app.profile_id)?;

    let fs_config = FourSharedConfig {
        consumer_key: app.client_id.clone(),
        consumer_secret: SecretString::from(app.client_secret.clone()),
        access_token: SecretString::from(access_token),
        access_token_secret: SecretString::from(access_token_secret),
    };

    let mut p = FourSharedProvider::new(fs_config);
    p.connect().await.map_err(|e| format!("4shared: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to 4shared");
    Ok(opened_oauth(Box::new(p), &app))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ProviderError, RemoteEntry};

    /// The identity main computes for an unpadded overlay profile
    /// (`a_padded_overlay_root_gets_its_own_identity_and_no_other_changes`).
    const MAIN_PLAIN_OVERLAY_IDENTITY: &str =
        "b508e9171a954fd9671fc8f22109b84c99ad38842d735dd62e79c27c58e84fd3";

    #[test]
    fn receipt_identity_follows_resolved_target_even_when_profile_labels_do_not_change() {
        let original = ProviderConfig {
            name: "same saved profile".into(),
            provider_type: ProviderType::S3,
            host: "first.example".into(),
            port: Some(443),
            username: Some("access-id".into()),
            password: Some("not fingerprinted".into()),
            initial_path: Some("/root".into()),
            extra: HashMap::from([
                ("bucket".into(), "first-bucket".into()),
                ("region".into(), "first-region".into()),
            ]),
        };
        let binding = serde_json::json!({"enabled":true,"remoteScope":"/crypt","id":"key-one"});
        let expected = resolved_target_identity(&original, &binding, Some("account-one")).unwrap();
        for change in [
            "host", "port", "username", "root", "bucket", "region", "endpoint",
        ] {
            let mut changed = original.clone();
            match change {
                "host" => changed.host = "second.example".into(),
                "port" => changed.port = Some(9443),
                "username" => changed.username = Some("other-access-id".into()),
                "root" => changed.initial_path = Some("/other".into()),
                option => {
                    changed.extra.insert(option.into(), "different".into());
                }
            }
            assert_ne!(
                resolved_target_identity(&changed, &binding, Some("account-one")).unwrap(),
                expected,
                "{change}"
            );
        }
        assert_ne!(
            resolved_target_identity(&original, &binding, Some("account-two")).unwrap(),
            expected
        );
        assert_ne!(
            resolved_target_identity(
                &original,
                &serde_json::json!({"enabled":true,"remoteScope":"/other"}),
                Some("account-one")
            )
            .unwrap(),
            expected
        );
        let mut rotated_secret = original.clone();
        rotated_secret.password = Some("new password".into());
        rotated_secret
            .extra
            .insert("key_passphrase".into(), "secret".into());
        assert_eq!(
            resolved_target_identity(&rotated_secret, &binding, Some("account-one")).unwrap(),
            expected
        );
        let mut unknown = original.clone();
        unknown.username = None;
        assert!(resolved_target_identity(&unknown, &binding, None).is_none());
        assert!(
            resolved_target_identity(&unknown, &binding, Some("known actual account")).is_some()
        );
        let mut bridge = original.clone();
        bridge.provider_type = ProviderType::Proton;
        bridge.username = Some("placeholder login".into());
        assert!(resolved_target_identity(&bridge, &binding, None).is_none());
        assert!(resolved_target_identity(&bridge, &binding, Some("actual account")).is_some());
        for (kind, selector) in [
            (ProviderType::Uploadcare, "public_key"),
            (ProviderType::KDrive, "drive_id"),
        ] {
            let mut namespace = unknown.clone();
            namespace.provider_type = kind;
            namespace
                .extra
                .insert(selector.into(), "first-namespace".into());
            let first = resolved_target_identity(&namespace, &binding, None).unwrap();
            namespace
                .extra
                .insert(selector.into(), "other-namespace".into());
            assert_ne!(
                resolved_target_identity(&namespace, &binding, None).unwrap(),
                first
            );
        }
        let mut display_only = original;
        display_only.provider_type = ProviderType::ImageKit;
        display_only
            .extra
            .insert("imagekit_id".into(), "display-url-only".into());
        assert!(resolved_target_identity(&display_only, &binding, None).is_none());
    }

    #[test]
    fn behavior_and_one_time_auth_inputs_do_not_change_namespace_identity() {
        let mut config = ProviderConfig {
            name: "label".into(),
            provider_type: ProviderType::S3,
            host: "server".into(),
            port: None,
            username: Some("account".into()),
            password: Some("secret".into()),
            initial_path: Some("/root".into()),
            extra: HashMap::from([("bucket".into(), "data".into())]),
        };
        let binding = serde_json::json!({"enabled":true,"remoteScope":"/crypt"});
        let before = resolved_target_identity(&config, &binding, None).unwrap();
        for key in [
            "save_session",
            "logout_on_disconnect",
            "two_factor_code",
            "totp_secret",
            "timeout",
            "verify_cert",
            "retry_count",
            "storage_class",
        ] {
            config.extra.insert(key.into(), "changed".into());
        }
        config.port = Some(ProviderType::S3.default_port());
        config.name = "new label".into();
        let explicit = serde_json::json!({"enabled":true,"kind":"aerocrypt","remoteScope":"/crypt","filenameEncryption":"standard","directoryNameEncryption":true,"withHeader":false,"useDefaultSalt":false,"lastOpened":"volatile"});
        assert_eq!(
            resolved_target_identity(&config, &explicit, None).unwrap(),
            before
        );
        config.extra.insert("bucket".into(), "other".into());
        assert_ne!(
            resolved_target_identity(&config, &explicit, None).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn opened_yandex_and_oauth_use_authenticated_identity_and_preserve_unknown_as_none() {
        use std::sync::{Arc, Mutex};
        let user = Arc::new(Mutex::new(
            serde_json::json!({"uid":"uid-one","login":"display"}),
        ));
        let state = user.clone();
        let app = axum::Router::new().route(
            "/",
            axum::routing::get(move || {
                let user = state.clone();
                async move { axum::Json(serde_json::json!({"user":user.lock().unwrap().clone()})) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let profile = serde_json::json!({"id":"same-slot","initialPath":"/"});
        let oauth = OAuthApp {
            client_id: "app".into(),
            client_secret: "not-persisted".into(),
            profile_id: "same-slot".into(),
            region: None,
            root: Some("/".into()),
            overlay_binding: serde_json::Value::Null,
        };
        let open =
            |url: &str| YandexDiskProvider::new("token".into(), None).with_test_api_base(url);
        let first = open_yandex_oauth(open(&endpoint), "same-slot", Some(&profile), "/cloud/")
            .await
            .unwrap();
        assert!(first.identity.is_some());
        assert_eq!(first.provider.account_email().as_deref(), Some("display"));
        assert_eq!(
            first.provider.sync_namespace_identity().as_deref(),
            Some("uid-one")
        );
        let generic = opened_oauth(first.provider, &oauth);
        assert!(generic.identity.is_some());
        *user.lock().unwrap() = serde_json::json!({"uid":"uid-two","login":"display"});
        let second = open_yandex_oauth(open(&endpoint), "same-slot", Some(&profile), "/cloud/")
            .await
            .unwrap();
        assert_ne!(second.identity, first.identity);
        assert_ne!(
            opened_oauth(second.provider, &oauth).identity,
            generic.identity
        );
        *user.lock().unwrap() = serde_json::Value::Null;
        let unknown = open_yandex_oauth(open(&endpoint), "same-slot", Some(&profile), "/cloud/")
            .await
            .unwrap();
        assert!(unknown.identity.is_none());
        assert!(opened_oauth(unknown.provider, &oauth).identity.is_none());
        server.abort();
    }

    #[test]
    fn test_parse_server_field_hostname_only() {
        let (host, port) = parse_server_field("nas.example.net");
        assert_eq!(host, "nas.example.net");
        assert_eq!(port, None);
    }

    #[test]
    fn test_parse_server_field_with_port() {
        let (host, port) = parse_server_field("nas.example.net:22");
        assert_eq!(host, "nas.example.net");
        assert_eq!(port, Some(22));
    }

    #[test]
    fn test_parse_server_field_webdav_path_no_port() {
        let (host, port) = parse_server_field("nas.example.net/user/dav");
        assert_eq!(host, "nas.example.net/user/dav");
        assert_eq!(port, None);
    }

    #[test]
    fn test_parse_server_field_webdav_port_after_path() {
        // CloudPanel bug: port appended after path
        let (host, port) = parse_server_field("nas.example.net/user/dav:80");
        assert_eq!(host, "nas.example.net/user/dav");
        assert_eq!(port, Some(80));
    }

    #[test]
    fn test_parse_server_field_webdav_port_after_host() {
        // Correct format: port after hostname before path
        let (host, port) = parse_server_field("nas.example.net:80/user/dav");
        assert_eq!(host, "nas.example.net/user/dav");
        assert_eq!(port, Some(80));
    }

    #[test]
    fn test_parse_server_field_ip_with_port() {
        let (host, port) = parse_server_field("192.168.1.100:2222");
        assert_eq!(host, "192.168.1.100");
        assert_eq!(port, Some(2222));
    }

    #[test]
    fn test_parse_server_field_ip_no_port() {
        let (host, port) = parse_server_field("192.168.1.100");
        assert_eq!(host, "192.168.1.100");
        assert_eq!(port, None);
    }

    // A3-07: IPv6 address support
    #[test]
    fn test_parse_server_field_ipv6_with_port() {
        let (host, port) = parse_server_field("[::1]:22");
        assert_eq!(host, "::1");
        assert_eq!(port, Some(22));
    }

    #[test]
    fn test_parse_server_field_ipv6_no_port() {
        let (host, port) = parse_server_field("[2001:db8::1]");
        assert_eq!(host, "2001:db8::1");
        assert_eq!(port, None);
    }

    #[test]
    fn test_parse_server_field_ipv6_full_with_port() {
        let (host, port) = parse_server_field("[fe80::1%25eth0]:8080");
        assert_eq!(host, "fe80::1%25eth0");
        assert_eq!(port, Some(8080));
    }

    /// Every existing Yandex Disk AeroCloud config is matched by this digest:
    /// a different value here changes every such identity, and each config
    /// would start a fresh index. The golden values were computed by hand from
    /// origin/main at 94dc427a7 (profile_root returning a String, the
    /// identity built from `profile.map(profile_root)`): the same canonical
    /// JSON array (compact, keys sorted, as `identity_digest` serializes it),
    /// SHA-256 in lowercase hex.
    #[test]
    fn yandex_identity_digest_is_unchanged_for_existing_configs() {
        let digest = |profile: Option<serde_json::Value>| {
            // The configs these digests stand for keep an absolute remote
            // folder (the wizard's `/cloud/`), so the identity keeps main's
            // trimmed root; the padded cases below are absolute ones too.
            let root = profile
                .as_ref()
                .map(|profile| identity_root(profile, "/cloud/"))
                .transpose()
                .unwrap();
            yandex_identity(
                "yd-1",
                root.as_deref(),
                "alice@example.com",
                &overlay_binding(profile.as_ref()),
            )
        };
        let cases = [
            (
                Some(serde_json::json!({ "initialPath": "/Photos" })),
                "94544ea7ca48f51c1a58de426f9141ec32db179928c78043c37106f48677ceb5",
            ),
            (
                Some(serde_json::json!({ "initialPath": "" })),
                "abed7ccfc7846c1bce6c605e172d58cae5014fca779db6025c259f7eea102ea7",
            ),
            (
                Some(serde_json::json!({
                    "initialPath": "/users/{username}/sync",
                    "username": "alice"
                })),
                "872e631de77d4cc85a77aadaf0892c9d7a0f2d2da709eefb18237b7e65f36713",
            ),
            (
                None,
                "c1f0f12f753dde975556797d5ea50e61d0dc4850ffedb24663a65435ddd27052",
            ),
            // Folders with edge spaces: main trimmed them, so their indexes
            // carry the trimmed root (the first one is `/Photos` above).
            (
                Some(serde_json::json!({ "initialPath": " /Photos " })),
                "94544ea7ca48f51c1a58de426f9141ec32db179928c78043c37106f48677ceb5",
            ),
            (
                Some(serde_json::json!({
                    "initialPath": "/users/{username}",
                    "username": "alice "
                })),
                "2ec264e82135bf84547c770bb336cfd1422d2a500246a806cea6384c73a3396d",
            ),
        ];
        for (profile, golden) in cases {
            assert_eq!(
                digest(profile.clone()),
                golden,
                "the Yandex Disk AeroCloud identity changed for {profile:?}: every existing \
                 config would get a new identity and start a fresh index"
            );
        }
    }

    fn root_ok(profile: serde_json::Value) -> String {
        profile_root(&profile).expect("a resolvable root")
    }

    #[test]
    fn a_slash_remote_folder_follows_the_root_only_where_slash_is_the_starting_folder() {
        let before =
            |protocol: &str| serde_json::json!({ "protocol": protocol, "initialPath": "/reports" });
        let after = |protocol: &str| serde_json::json!({ "protocol": protocol, "initialPath": "/reports " });
        // S3: `/` is the bucket prefix the root names, so a padded root is
        // another prefix and another identity; an unpadded one keeps main's.
        assert_ne!(
            identity_root(&before("s3"), "/").unwrap(),
            identity_root(&after("s3"), "/").unwrap()
        );
        assert_eq!(identity_root(&before("s3"), "/").unwrap(), "/reports");
        assert!(!remote_folder_is_root_independent("/", &before("s3")));
        // An absolute folder below `/` names its own prefix on S3 too.
        assert!(remote_folder_is_root_independent("/cloud/", &before("s3")));
        // WebDAV: `/` is the server root the connect keeps, the starting
        // folder as written.
        assert_ne!(
            identity_root(&before("webdav"), "/").unwrap(),
            identity_root(&after("webdav"), "/").unwrap()
        );
        assert_eq!(identity_root(&before("webdav"), "/").unwrap(), "/reports");
        assert!(!remote_folder_is_root_independent("/", &before("webdav")));
        // `/` is the container, bucket or server root everywhere else.
        for protocol in [
            "azure",
            "swift",
            "backblaze",
            "ftp",
            "ftps",
            "sftp",
            "scp",
            "cloudinary",
            "imagekit",
            "googledrive",
        ] {
            assert!(
                remote_folder_is_root_independent("/", &before(protocol)),
                "{protocol}"
            );
            assert_eq!(
                identity_root(&before(protocol), "/").unwrap(),
                identity_root(&after(protocol), "/").unwrap(),
                "{protocol}"
            );
        }
    }

    /// What a WebDAV `/` lists, on the wire: the connect keeps the starting
    /// folder as its server root, so two roots that differ by one space list
    /// two folders, and their identities differ with them.
    #[tokio::test]
    async fn a_webdav_slash_lists_the_root_the_identity_names() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // A WebDAV server where every path is a folder; it records the path
        // of each PROPFIND.
        async fn dav_server() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let paths = seen.clone();
            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let paths = paths.clone();
                    tokio::spawn(async move {
                        loop {
                            let mut request = Vec::new();
                            let mut block = [0u8; 4096];
                            let end = loop {
                                match stream.read(&mut block).await {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => request.extend_from_slice(&block[..n]),
                                }
                                if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                                    break i + 4;
                                }
                            };
                            let head = String::from_utf8_lossy(&request[..end]).to_string();
                            let length = head
                                .lines()
                                .find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            while request.len() < end + length {
                                match stream.read(&mut block).await {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => request.extend_from_slice(&block[..n]),
                                }
                            }
                            let mut words = head.split_whitespace();
                            let method = words.next().unwrap_or("").to_string();
                            let path = words.next().unwrap_or("/").to_string();
                            let reply = if method == "PROPFIND" {
                                paths.lock().unwrap().push(path.clone());
                                let body = format!(
                                    "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>{path}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"
                                );
                                format!("HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\n\r\n{}", body.len(), body)
                            } else {
                                "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n"
                                    .to_string()
                            };
                            if stream.write_all(reply.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                    });
                }
            });
            (url, seen)
        }
        // The profile's root entered as the factory enters it, then the
        // config's remote folder `/`: the folder that `/` lists, and the path
        // of the last PROPFIND, the listing of `/`.
        async fn slash_on_webdav(initial: &str) -> (String, String) {
            let (url, seen) = dav_server().await;
            let profile = serde_json::json!({ "protocol": "webdav", "initialPath": initial });
            let root = profile_root(&profile).unwrap();
            let config = ProviderConfig {
                name: "dav".into(),
                provider_type: ProviderType::WebDav,
                host: url,
                port: None,
                username: Some("u".into()),
                password: Some("p".into()),
                initial_path: Some(root.clone()),
                extra: HashMap::new(),
            };
            tokio::time::timeout(std::time::Duration::from_secs(20), async {
                let mut provider = crate::providers::ProviderFactory::create(&config).unwrap();
                provider.connect().await.unwrap();
                enter_root_once(&mut *provider, &root).await.unwrap();
                provider.cd("/").await.unwrap();
                let here = provider.pwd().await.unwrap();
                provider.list("/").await.unwrap();
                let listed = seen.lock().unwrap().last().cloned().unwrap();
                (here, listed)
            })
            .await
            .expect("the WebDAV exchange finished")
        }
        let (plain_here, plain_listed) = slash_on_webdav("/reports").await;
        let (padded_here, padded_listed) = slash_on_webdav("/reports ").await;
        assert_eq!(plain_here, "/reports");
        assert_eq!(padded_here, "/reports ");
        assert_ne!(plain_listed, padded_listed);
        assert!(plain_listed.starts_with("/reports"), "{plain_listed}");
        let identity = |initial: &str| {
            identity_root(
                &serde_json::json!({ "protocol": "webdav", "initialPath": initial }),
                "/",
            )
            .unwrap()
        };
        // Two folders, two identities; the unpadded one keeps main's form.
        assert_eq!(identity("/reports"), "/reports");
        assert_eq!(identity("/reports "), "/reports ");
    }

    #[test]
    fn the_root_is_entered_once_whoever_opened_it() {
        use RootStep::{Confirm, Enter, Nothing};
        let cases = [
            // An absolute root is always entered: that checks it exists.
            ("/reports", "/reports", Enter),
            ("/reports/", "/reports", Enter),
            ("/reports", "/", Enter),
            ("/reports", "/home/u", Enter),
            // A relative root the provider placed is confirmed by the folder
            // it reports; one it did not place is entered.
            (" /reports", "/home/u/ /reports", Confirm),
            ("~/docs", "/home/u/docs", Confirm),
            ("~/docs", "/home/u", Enter),
            ("docs", "/home/u/docs", Confirm),
            ("docs", "/home/u", Enter),
            ("docs", "/", Enter),
            ("docs", "", Enter),
            ("a/b", "/x/a/b", Confirm),
            ("b", "/x/ab", Enter),
            ("/", "/home/u", Nothing),
            ("  ", "/", Nothing),
        ];
        for (root, here, step) in cases {
            assert_eq!(root_step(root, here), step, "{root:?} at {here:?}");
        }
    }

    /// A provider whose connect sets the folder it reports without asking
    /// the server, as SFTP's does, and whose `cd` fails on a missing folder.
    struct Stands {
        here: String,
        folders: Vec<&'static str>,
        cds: Vec<String>,
    }

    #[async_trait::async_trait]
    impl StorageProvider for Stands {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> ProviderType {
            ProviderType::Sftp
        }
        fn display_name(&self) -> String {
            "stands".into()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(&mut self, _p: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
            Ok(vec![])
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok(self.here.clone())
        }
        async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
            self.cds.push(path.to_string());
            let target = if path.starts_with('/') {
                path.to_string()
            } else {
                format!("{}/{path}", self.here.trim_end_matches('/'))
            };
            if !self.folders.contains(&target.as_str()) {
                return Err(ProviderError::NotFound(target));
            }
            self.here = target;
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            _r: &str,
            _l: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download_to_bytes(&mut self, _r: &str) -> Result<Vec<u8>, ProviderError> {
            Ok(vec![])
        }
        async fn upload(
            &mut self,
            _l: &str,
            _r: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn mkdir(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rename(&mut self, _f: &str, _t: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn stat(&mut self, _p: &str) -> Result<RemoteEntry, ProviderError> {
            Err(ProviderError::NotSupported("stat".into()))
        }
        async fn size(&mut self, _p: &str) -> Result<u64, ProviderError> {
            Ok(0)
        }
        async fn exists(&mut self, _p: &str) -> Result<bool, ProviderError> {
            Ok(true)
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("stands".into())
        }
    }

    /// The connect fails on a root that is not there, also where the provider
    /// reports standing in it without having asked the server.
    #[tokio::test]
    async fn a_missing_root_fails_the_connect_where_the_provider_did_not_check() {
        let stands = |here: &str, folders: Vec<&'static str>| Stands {
            here: here.to_string(),
            folders,
            cds: Vec::new(),
        };
        // Absolute and relative, the provider standing "in" a deleted root.
        for (root, here) in [("/data/gone", "/data/gone"), ("docs", "/home/u/docs")] {
            let mut provider = stands(here, vec!["/home/u", "/data"]);
            assert!(
                enter_root_once(&mut provider, root).await.is_err(),
                "{root}"
            );
        }
        // Present: an absolute root is entered as written, a placed relative
        // one is confirmed by its absolute path and never entered again.
        let mut provider = stands("/data/set", vec!["/data/set"]);
        enter_root_once(&mut provider, "/data/set").await.unwrap();
        assert_eq!(provider.cds, ["/data/set"]);
        let mut provider = stands("/home/u/docs", vec!["/home/u/docs"]);
        enter_root_once(&mut provider, "docs").await.unwrap();
        assert_eq!(provider.cds, ["/home/u/docs"]);
        // A provider that ignored the root and stands in its login folder.
        let mut provider = stands("/home/u", vec!["/home/u", "/home/u/docs"]);
        enter_root_once(&mut provider, "docs").await.unwrap();
        assert_eq!(provider.cds, ["docs"]);
        assert_eq!(provider.here, "/home/u/docs");
    }

    /// An FTP profile whose root is relative (a leading space) is entered
    /// once: the provider's connect places it, and the factory confirms the
    /// folder by its absolute path instead of entering the root again.
    #[tokio::test]
    async fn an_ftp_relative_root_is_entered_once_then_confirmed() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let cwds = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = cwds.clone();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut cwd = "/home/u".to_string();
            write.write_all(b"220 ready\r\n").await.unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                let (verb, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                let reply = match verb.to_ascii_uppercase().as_str() {
                    "USER" => "331 password\r\n".to_string(),
                    "PASS" => "230 logged in\r\n".to_string(),
                    "CWD" => {
                        seen.lock().unwrap().push(arg.to_string());
                        cwd = if arg.starts_with('/') {
                            arg.to_string()
                        } else {
                            format!("{cwd}/{arg}")
                        };
                        "250 ok\r\n".to_string()
                    }
                    "PWD" => format!("257 \"{cwd}\"\r\n"),
                    "FEAT" => "211-Features:\r\n211 End\r\n".to_string(),
                    "QUIT" => {
                        let _ = write.write_all(b"221 bye\r\n").await;
                        break;
                    }
                    _ => "200 ok\r\n".to_string(),
                };
                if write.write_all(reply.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let profile = serde_json::json!({ "protocol": "ftp", "initialPath": " /reports" });
        let root = profile_root(&profile).unwrap();
        let config = ProviderConfig {
            name: "relative".into(),
            provider_type: ProviderType::Ftp,
            host: "127.0.0.1".into(),
            port: Some(port),
            username: Some("u".into()),
            password: Some("p".into()),
            initial_path: Some(root.clone()),
            extra: HashMap::new(),
        };
        // Bounded: a reply the client does not expect must fail the test,
        // not hang the suite.
        let here = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let mut provider = crate::providers::ProviderFactory::create(&config).unwrap();
            provider.connect().await.unwrap();
            enter_root_once(&mut *provider, &root).await.unwrap();
            provider.pwd().await.unwrap()
        })
        .await
        .expect("the FTP exchange finished");
        // The connect enters the root; the factory confirms the folder the
        // server reports, by its absolute path, and does not enter it again.
        assert_eq!(
            *cwds.lock().unwrap(),
            vec![" /reports".to_string(), "/home/u/ /reports".to_string()]
        );
        assert_eq!(here, "/home/u/ /reports");
    }

    /// The factory connect enters the root through `enter_root_once`, which
    /// the tests above run, and never directly.
    #[test]
    fn the_factory_connect_enters_the_root_once() {
        let source = include_str!("cloud_provider_factory.rs");
        let start = source.find("async fn create_via_factory(").unwrap();
        let body = &source[start..start + source[start..].find("\n}\n").unwrap()];
        assert_eq!(
            body.matches("enter_root_once(&mut *provider, root)")
                .count(),
            1
        );
        assert!(!body.contains("enter_profile_root("), "{body}");
    }

    /// With no `remoteScope` the overlay sits at the starting folder as
    /// written, while the identity of an absolute remote folder keeps the root
    /// trimmed: `/reports ` and `/reports` were one identity for two encrypted
    /// folders. The anchor joins the identity only there, so an unpadded
    /// profile keeps main's identity byte for byte, and only a padded-root
    /// overlay profile gets a new one.
    #[test]
    fn a_padded_overlay_root_gets_its_own_identity_and_no_other_changes() {
        let profile = |initial: &str, overlay: serde_json::Value| {
            serde_json::json!({
                "protocol": "sftp", "host": "nas.example.test", "username": "u",
                "initialPath": initial, "aeroCryptOverlay": overlay,
            })
        };
        let overlay = serde_json::json!({ "enabled": true, "kind": "rclone-crypt" });
        let config = |profile: &serde_json::Value| ProviderConfig {
            name: "p".into(),
            provider_type: ProviderType::Sftp,
            host: "nas.example.test".into(),
            port: Some(22),
            username: Some("u".into()),
            password: None,
            initial_path: Some(identity_root(profile, "/cloud/").unwrap()),
            extra: HashMap::new(),
        };
        // As the factory builds it now, and as main built it (the binding alone).
        let now = |profile: &serde_json::Value| {
            let binding = overlay_binding(Some(profile));
            resolved_target_identity(
                &config(profile),
                &identity_overlay_binding(Some(profile), "/cloud/", &binding),
                Some("acct"),
            )
            .unwrap()
        };
        let main = |profile: &serde_json::Value| {
            resolved_target_identity(
                &config(profile),
                &overlay_binding(Some(profile)),
                Some("acct"),
            )
            .unwrap()
        };

        let plain = profile("/reports", overlay.clone());
        assert_eq!(now(&plain), main(&plain));
        assert_eq!(now(&plain), MAIN_PLAIN_OVERLAY_IDENTITY);
        // No overlay, or a scope of its own: unchanged whatever the padding.
        for unchanged in [
            profile("/reports ", serde_json::Value::Null),
            profile(
                "/reports ",
                serde_json::json!({ "enabled": true, "remoteScope": "/crypt" }),
            ),
        ] {
            assert_eq!(now(&unchanged), main(&unchanged), "{unchanged}");
        }
        // Padded roots: a new identity each, never shared with another folder.
        let identities: Vec<String> = ["/reports", "/reports ", " /reports", "/reports  "]
            .into_iter()
            .map(|initial| now(&profile(initial, overlay.clone())))
            .collect();
        let distinct: std::collections::HashSet<&String> = identities.iter().collect();
        assert_eq!(distinct.len(), identities.len(), "{identities:?}");
    }

    /// The identity follows the data path. With an absolute remote folder
    /// (or one under the SFTP login folder) the data path does not depend on
    /// the root, so the identity keeps main's form (filled, trimmed, placed)
    /// and nobody re-indexes; with a relative or blank one the data path is
    /// the literal root plus that folder, so the identity carries the literal
    /// root. The data path is resolved as `cd` resolves it
    /// (`resolve_remote_path` from the entered root). The factory digests are
    /// the ones main computed, hashed by hand from main's
    /// `resolved_target_identity` (canonical JSON, SHA-256).
    #[test]
    fn identity_root_follows_the_data_path() {
        let opened = |profile: &serde_json::Value, remote: &str| {
            crate::providers::sftp::resolve_remote_path(
                &profile_root(profile).unwrap(),
                "/home/u",
                remote,
            )
        };
        // Root-independent remote folders: main's trimmed form.
        let absolute = [
            (
                serde_json::json!({ "initialPath": "/reports " }),
                "/cloud/",
                "/reports",
                "/reports ",
            ),
            (
                serde_json::json!({ "initialPath": " /reports" }),
                "/cloud/",
                "/reports",
                " /reports",
            ),
            (
                serde_json::json!({ "initialPath": "/users/{username}", "username": "alice " }),
                "/cloud/",
                "/users/alice",
                "/users/alice ",
            ),
            (
                serde_json::json!({ "protocol": "sftp", "initialPath": "docs " }),
                "~/cloud",
                "~/docs",
                "~/docs ",
            ),
            (
                serde_json::json!({ "protocol": "sftp", "initialPath": " docs" }),
                "/cloud/",
                "~/docs",
                "~/ docs",
            ),
            (
                serde_json::json!({ "initialPath": "  " }),
                "/cloud/",
                "/",
                "/",
            ),
        ];
        for (profile, remote, identity, root) in absolute {
            assert_eq!(
                identity_root(&profile, remote).unwrap(),
                identity,
                "{profile} {remote}"
            );
            assert_eq!(profile_root(&profile).unwrap(), root, "{profile}");
        }
        // Folders opened under the root: the literal root. `~/cloud` is
        // under the login folder only on SFTP and SCP.
        for (profile, remote, identity) in [
            (
                serde_json::json!({ "initialPath": "/reports " }),
                "cloud/",
                "/reports ",
            ),
            (
                serde_json::json!({ "initialPath": "/reports " }),
                "",
                "/reports ",
            ),
            (
                serde_json::json!({ "initialPath": "/reports " }),
                "~/cloud",
                "/reports ",
            ),
            (
                serde_json::json!({ "initialPath": "/users/{username}", "username": "alice " }),
                "cloud/",
                "/users/alice ",
            ),
        ] {
            assert_eq!(
                identity_root(&profile, remote).unwrap(),
                identity,
                "{profile} {remote:?}"
            );
        }

        // One profile whose folder is edited by one trailing space.
        let before = serde_json::json!({ "protocol": "webdav", "initialPath": "/reports" });
        let after = serde_json::json!({ "protocol": "webdav", "initialPath": "/reports " });
        // Absolute remote folder: the same data path, so the same identity.
        assert_eq!(opened(&before, "/cloud/"), opened(&after, "/cloud/"));
        assert_eq!(
            identity_root(&before, "/cloud/").unwrap(),
            identity_root(&after, "/cloud/").unwrap()
        );
        // Relative remote folder: another data path, so another identity.
        assert_eq!(opened(&before, "cloud"), "/reports/cloud");
        assert_eq!(opened(&after, "cloud"), "/reports /cloud");
        assert_ne!(
            identity_root(&before, "cloud").unwrap(),
            identity_root(&after, "cloud").unwrap()
        );

        let config = |root: String| ProviderConfig {
            name: "padded".into(),
            provider_type: ProviderType::WebDav,
            host: "dav.example.test".into(),
            port: Some(443),
            username: Some("alice".into()),
            password: None,
            initial_path: Some(root),
            extra: HashMap::new(),
        };
        let binding = serde_json::Value::Null;
        for (folder, user, remote, golden) in [
            // Absolute remote folders: padded folders keep main's digest.
            (
                "/reports ",
                "alice",
                "/cloud/",
                "72d819bbedf5054f7aa4614af41d575cd609561d8f55569c693cca6827abbac9",
            ),
            (
                "/users/{username}",
                "alice ",
                "/cloud/",
                "c2874d1d749ec91b678a2429a5b9c654c560f1d24667ed20b304ec5dcbdee950",
            ),
            // A relative remote folder under an unpadded root keeps main's
            // digest too: existing relative installs do not re-index.
            (
                "/reports",
                "alice",
                "cloud/",
                "72d819bbedf5054f7aa4614af41d575cd609561d8f55569c693cca6827abbac9",
            ),
        ] {
            let profile = serde_json::json!({
                "protocol": "webdav",
                "initialPath": folder,
                "username": user,
            });
            let root = identity_root(&profile, remote).unwrap();
            assert_eq!(
                resolved_target_identity(&config(root), &binding, Some("account-one")).unwrap(),
                golden,
                "the AeroCloud identity changed for {folder:?} with {remote:?}: the install would start a fresh index"
            );
        }
    }

    #[test]
    fn profile_root_is_the_starting_folder_with_the_username_resolved() {
        assert_eq!(root_ok(serde_json::json!({})), "/");
        assert_eq!(root_ok(serde_json::json!({ "initialPath": "  " })), "/");
        assert_eq!(
            root_ok(serde_json::json!({ "initialPath": "/var/www" })),
            "/var/www"
        );
        assert_eq!(
            root_ok(serde_json::json!({
                "initialPath": "/files/{username}/",
                "username": "ale"
            })),
            "/files/ale/"
        );
        // SFTP and SCP open a relative folder under the login folder.
        for protocol in ["sftp", "scp"] {
            assert_eq!(
                root_ok(serde_json::json!({ "protocol": protocol, "initialPath": "docs" })),
                "~/docs"
            );
            assert_eq!(
                root_ok(serde_json::json!({ "protocol": protocol, "initialPath": "/srv" })),
                "/srv"
            );
            assert_eq!(
                root_ok(serde_json::json!({ "protocol": protocol, "initialPath": "~/v" })),
                "~/v"
            );
        }
        assert_eq!(
            root_ok(serde_json::json!({ "protocol": "ftp", "initialPath": "docs" })),
            "docs"
        );
    }

    /// AeroCloud reads its root from the shared case table like every other
    /// reader: the user name as stored, or the template when it is refused.
    #[test]
    fn profile_root_fills_the_username_from_the_shared_table() {
        let table: serde_json::Value = serde_json::from_str(include_str!(
            "../../src/shared/username-template-cases.json"
        ))
        .unwrap();
        let template = table["pathTemplate"].as_str().unwrap();
        for case in table["cases"].as_array().unwrap() {
            let user = case["user"].as_str().unwrap();
            let got =
                profile_root(&serde_json::json!({ "initialPath": template, "username": user }));
            match case["path"].as_str() {
                Some(expected) => assert_eq!(
                    got,
                    profile_root(&serde_json::json!({ "initialPath": expected })),
                    "{user:?}"
                ),
                // A root that cannot be filled is refused, never synced as a
                // folder named after the placeholder.
                None => assert_eq!(
                    got,
                    Err(crate::connection_manifest::USERNAME_TEMPLATE_UNRESOLVED.to_string()),
                    "{user:?}"
                ),
            }
        }
    }

    #[test]
    fn connection_extra_carries_the_profile_options() {
        let profile = serde_json::json!({
            "id": "srv_b2",
            "name": "B2",
            "options": { "bucket": "photos" }
        });

        let (extra, _) = connection_extra(
            Some(&profile),
            ProviderType::Backblaze,
            "",
            &serde_json::Value::Null,
        );

        assert_eq!(
            extra.get("bucket").map(String::as_str),
            Some("photos"),
            "the bucket of the saved profile must reach the provider"
        );
        assert_eq!(extra.get("profile_id").map(String::as_str), Some("srv_b2"));
    }

    #[test]
    fn connection_extra_maps_azure_container_and_sftp_key_options() {
        let azure = serde_json::json!({
            "id": "srv_az",
            "name": "Azure",
            "options": { "bucket": "backups" }
        });
        let (extra, _) = connection_extra(
            Some(&azure),
            ProviderType::Azure,
            "account",
            &serde_json::Value::Null,
        );
        assert_eq!(
            extra.get("container").map(String::as_str),
            Some("backups"),
            "Azure stores its container as bucket"
        );

        let sftp = serde_json::json!({
            "id": "srv_sftp",
            "name": "NAS",
            "options": {
                "privateKeyPath": "~/.ssh/aerocloud_ed25519",
                "keyPassphrase": "secret"
            }
        });
        let (extra, _) = connection_extra(
            Some(&sftp),
            ProviderType::Sftp,
            "nas.example.net",
            &serde_json::Value::Null,
        );
        assert_eq!(
            extra.get("private_key_path").map(String::as_str),
            Some("~/.ssh/aerocloud_ed25519")
        );
        assert_eq!(
            extra.get("key_passphrase").map(String::as_str),
            Some("secret")
        );
    }

    #[test]
    fn connection_extra_lets_legacy_params_win_but_never_the_profile_id() {
        let profile = serde_json::json!({
            "id": "srv_1",
            "name": "NAS",
            "options": { "private_key_path": "~/.ssh/profile_key" }
        });
        let params = serde_json::json!({
            "private_key_path": "/tmp/explicit_key",
            "profile_id": "srv_other",
            "port": 2222
        });

        let (extra, _) = connection_extra(Some(&profile), ProviderType::Sftp, "nas", &params);

        assert_eq!(
            extra.get("private_key_path").map(String::as_str),
            Some("/tmp/explicit_key")
        );
        assert_eq!(extra.get("port").map(String::as_str), Some("2222"));
        assert_eq!(
            extra.get("profile_id").map(String::as_str),
            Some("srv_1"),
            "a param must not redirect the per-profile token lookup"
        );
    }

    #[test]
    fn connection_extra_without_a_profile_is_the_params() {
        let params = serde_json::json!({ "bucket": "legacy", "port": "21" });

        let (extra, host) = connection_extra(None, ProviderType::S3, "s3.example.com", &params);

        assert_eq!(host, "s3.example.com");
        assert_eq!(extra.len(), 2);
        assert_eq!(extra.get("bucket").map(String::as_str), Some("legacy"));
    }

    /// Live survey of the AeroCloud connect path against every saved profile of
    /// the vault the test binary reads, built as the setup wizard stores it: the
    /// profile name, the profile's protocol and no connection params, because
    /// the profile is the only source of the connection. The remote folder is
    /// the profile's starting folder, where every other surface connects, so a
    /// failure here is the AeroCloud path and not a folder the sync would
    /// create. Each profile goes through `create_cloud_provider`, the crypt
    /// overlay stack the sync applies, and a read-only list of the AeroCloud
    /// base (the overlay folder, else the profile folder), which a bound
    /// overlay must decrypt. Nothing is written remotely.
    ///
    /// `AEROFTP_LIVE_AEROCLOUD_PROFILES` (comma separated names) narrows the run.
    /// For the production vault, build the crate without debug assertions:
    /// `cargo test --config 'profile.test.package.aeroftp.debug-assertions=false'`.
    ///
    /// A native AeroCrypt binding is reported SKIP, not tested: a test build
    /// derives AeroCrypt keys with reduced Argon2 parameters (`cfg(test)` in
    /// `crypto.rs` and the `aerocrypt` crate), so it can never open a real
    /// vault. Those profiles need a run of the real app.
    #[tokio::test]
    #[ignore = "live: connects every saved profile of the vault the binary reads"]
    async fn live_aerocloud_connects_saved_profiles() {
        use crate::credential_store::CredentialStore;
        use std::time::Duration;

        assert_eq!(CredentialStore::init().expect("open the vault"), "OK");
        let store = CredentialStore::from_cache().expect("the vault is open");
        let profiles = crate::user_partitions::mcp_list_active_server_profiles(&store)
            .expect("list the saved profiles");
        let only: Option<Vec<String>> = std::env::var("AEROFTP_LIVE_AEROCLOUD_PROFILES")
            .ok()
            .map(|v| v.split(',').map(|s| s.trim().to_string()).collect());

        let mut rows = Vec::new();
        for profile in &profiles {
            let name = profile.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let protocol = profile
                .get("protocol")
                .and_then(|v| v.as_str())
                .unwrap_or("ftp");
            if only.as_ref().is_some_and(|o| !o.iter().any(|n| n == name)) {
                continue;
            }
            if crate::crypt_overlay_provider::profile_crypt_overlay_kind(profile)
                == Some("aerocrypt")
            {
                rows.push((protocol.to_string(), name.to_string(), None));
                continue;
            }
            let config = CloudConfig {
                enabled: true,
                cloud_name: "live survey".to_string(),
                server_profile: name.to_string(),
                server_profile_id: profile
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                protocol_type: protocol.to_string(),
                connection_params: serde_json::json!({}),
                remote_folder: profile
                    .get("aeroCryptOverlay")
                    .filter(|o| o.get("enabled").and_then(|v| v.as_bool()) == Some(true))
                    .and_then(|o| o.get("remoteScope"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| profile_root(profile).expect("a resolvable root")),
                ..CloudConfig::default()
            };
            let outcome = tokio::time::timeout(Duration::from_secs(90), async {
                let provider = create_cloud_provider(&config).await?;
                let mut provider = crate::crypt_overlay_provider::build_aerocloud_overlay_stack(
                    provider,
                    config.server_profile_id.as_deref(),
                    name,
                    &store,
                )
                .await?;
                let listed = provider
                    .list(&config.remote_folder)
                    .await
                    .map_err(|e| e.to_string());
                let _ = provider.disconnect().await;
                listed.map(|entries| entries.len())
            })
            .await
            .unwrap_or_else(|_| Err("timed out after 90 s".to_string()));
            rows.push((protocol.to_string(), name.to_string(), Some(outcome)));
        }

        rows.sort();
        let mut failed = Vec::new();
        for (protocol, name, outcome) in &rows {
            match outcome {
                None => {
                    eprintln!("SKIP  {protocol:<14} {name}: AeroCrypt keys need a non-test build")
                }
                Some(Ok(n)) => eprintln!("OK    {protocol:<14} {name} ({n} entries)"),
                Some(Err(e)) => {
                    eprintln!("FAIL  {protocol:<14} {name}: {e}");
                    failed.push(format!("{protocol} '{name}': {e}"));
                }
            }
        }
        assert!(!rows.is_empty(), "no saved profile matched");
        assert!(
            failed.is_empty(),
            "AeroCloud could not connect {} of {} saved profiles:\n{}",
            failed.len(),
            rows.len(),
            failed.join("\n")
        );
    }
}
