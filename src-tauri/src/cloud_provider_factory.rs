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
    match config.protocol_type.as_str() {
        // --- Direct auth providers: use ProviderFactory ---
        "ftp" => create_via_factory(config, ProviderType::Ftp).await,
        "ftps" => create_via_factory(config, ProviderType::Ftps).await,
        "sftp" => create_via_factory(config, ProviderType::Sftp).await,
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
/// remote folder (which may not exist yet) inside it.
fn profile_root(profile: &serde_json::Value) -> String {
    let username = profile
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let root = profile
        .get("initialPath")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .replace("{username}", username);
    let root = root.trim();
    if root.is_empty() {
        "/".to_string()
    } else {
        root.to_string()
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
) -> Result<Box<dyn StorageProvider>, String> {
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
                .unwrap_or_else(|| config.remote_folder.clone()),
        ),
        extra,
    };

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

    info!("AeroCloud: connecting via {:?} to {}", provider_type, host);
    provider
        .connect()
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;
    // Not every provider starts in `initial_path` (Filen opens at the account
    // root), so the root is entered explicitly, as the CLI does before it wraps
    // a crypt overlay: the overlay unlocks where the provider stands.
    if let Some(profile) = &profile {
        enter_profile_root(&mut *provider, Some(&profile_root(profile))).await?;
    }

    Ok(provider)
}

/// Yandex Disk profiles either authorize with OAuth, the tokens living in the
/// profile's slot where the connect path and the CLI read them, or carry a
/// token typed into the password field. The OAuth tokens win when the profile
/// has them; otherwise the typed token connects through the factory.
async fn create_yandex(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
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
            let mut p = YandexDiskProvider::new(tokens.access_token.clone(), None);
            p.connect()
                .await
                .map_err(|e| format!("Yandex Disk: {}", e))?;
            enter_profile_root(&mut p, profile.as_ref().map(profile_root).as_deref()).await?;
            info!("AeroCloud: connected to Yandex Disk");
            return Ok(Box::new(p));
        }
    }
    create_via_factory(config, ProviderType::YandexDisk).await
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
        root: Some(profile_root(&profile)),
    })
}

async fn create_google_drive(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let app = oauth_app(config, "client_id", "client_secret")?;
    let gc = GoogleDriveConfig::new(&app.client_id, &app.client_secret);
    let mut p = GoogleDriveProvider::new(gc).with_profile_id(&app.profile_id);
    p.connect()
        .await
        .map_err(|e| format!("Google Drive: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to Google Drive");
    Ok(Box::new(p))
}

async fn create_dropbox(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let app = oauth_app(config, "client_id", "client_secret")?;
    let dc = DropboxConfig::new(&app.client_id, &app.client_secret);
    let mut p = DropboxProvider::new(dc).with_profile_id(&app.profile_id);
    p.connect().await.map_err(|e| format!("Dropbox: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to Dropbox");
    Ok(Box::new(p))
}

async fn create_onedrive(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let app = oauth_app(config, "client_id", "client_secret")?;
    let oc = OneDriveConfig::new(&app.client_id, &app.client_secret);
    let mut p = OneDriveProvider::new(oc).with_profile_id(&app.profile_id);
    p.connect().await.map_err(|e| format!("OneDrive: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to OneDrive");
    Ok(Box::new(p))
}

async fn create_box(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let app = oauth_app(config, "client_id", "client_secret")?;
    let bc = BoxConfig::new(&app.client_id, &app.client_secret);
    let mut p = BoxProvider::new(bc).with_profile_id(&app.profile_id);
    p.connect().await.map_err(|e| format!("Box: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to Box");
    Ok(Box::new(p))
}

async fn create_pcloud(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let app = oauth_app(config, "client_id", "client_secret")?;
    // pCloud tokens are region-locked: the region detected during the token
    // exchange (vault) wins, as in the normal connect path.
    let region = credential_store::CredentialStore::from_cache()
        .and_then(|store| store.get("oauth_pcloud_region").ok())
        .or(app.region)
        .unwrap_or_else(|| "us".to_string());
    let pc = PCloudConfig::new(&app.client_id, &app.client_secret, &region);
    let mut p = PCloudProvider::new(pc).with_profile_id(&app.profile_id);
    p.connect().await.map_err(|e| format!("pCloud: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to pCloud ({})", region);
    Ok(Box::new(p))
}

async fn create_zoho(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let app = oauth_app(config, "client_id", "client_secret")?;
    // The profile's region, then the one stored at authorization, as the normal
    // connect path resolves it.
    let region = app
        .region
        .or_else(|| {
            credential_store::CredentialStore::from_cache()
                .and_then(|store| store.get("oauth_zohoworkdrive_region").ok())
        })
        .unwrap_or_else(|| "us".to_string());
    let zc = ZohoWorkdriveConfig::new(&app.client_id, &app.client_secret, &region);
    let mut p = ZohoWorkdriveProvider::new(zc).with_profile_id(&app.profile_id);
    p.connect()
        .await
        .map_err(|e| format!("Zoho WorkDrive: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to Zoho WorkDrive ({})", region);
    Ok(Box::new(p))
}

// ── OAuth1 (4shared) ───────────────────────────────────────────────

async fn create_fourshared(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let app = oauth_app(config, "consumer_key", "consumer_secret")?;
    // The tokens live where the 4shared connect path stores them: the
    // profile's slot, or the provider singleton for a legacy config.
    let (access_token, access_token_secret) =
        crate::provider_commands::load_fourshared_tokens(&app.profile_id)?;

    let fs_config = FourSharedConfig {
        consumer_key: app.client_id,
        consumer_secret: SecretString::from(app.client_secret),
        access_token: SecretString::from(access_token),
        access_token_secret: SecretString::from(access_token_secret),
    };

    let mut p = FourSharedProvider::new(fs_config);
    p.connect().await.map_err(|e| format!("4shared: {}", e))?;
    enter_profile_root(&mut p, app.root.as_deref()).await?;
    info!("AeroCloud: connected to 4shared");
    Ok(Box::new(p))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn profile_root_is_the_starting_folder_with_the_username_resolved() {
        assert_eq!(profile_root(&serde_json::json!({})), "/");
        assert_eq!(
            profile_root(&serde_json::json!({ "initialPath": "  " })),
            "/"
        );
        assert_eq!(
            profile_root(&serde_json::json!({ "initialPath": "/var/www" })),
            "/var/www"
        );
        assert_eq!(
            profile_root(&serde_json::json!({
                "initialPath": "/files/{username}/",
                "username": "ale"
            })),
            "/files/ale/"
        );
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
                    .unwrap_or_else(|| profile_root(profile)),
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
