// AeroCloud Multi-Protocol Provider Factory
// Creates and connects a StorageProvider based on CloudConfig protocol_type

use crate::cloud_config::CloudConfig;
use crate::credential_store;
use crate::providers::{
    StorageProvider, ProviderFactory, ProviderConfig,
    types::{ProviderType, BoxConfig, PCloudConfig, FourSharedConfig},
    google_drive::{GoogleDriveProvider, GoogleDriveConfig},
    dropbox::{DropboxProvider, DropboxConfig},
    onedrive::{OneDriveProvider, OneDriveConfig},
    box_provider::BoxProvider,
    pcloud::PCloudProvider,
    zoho_workdrive::{ZohoWorkdriveProvider, ZohoWorkdriveConfig},
    fourshared::FourSharedProvider,
};
use secrecy::SecretString;
use tracing::info;

/// Create and connect a provider for AeroCloud background sync.
///
/// Dispatches based on `config.protocol_type`:
/// - Direct auth (FTP, SFTP, WebDAV, S3, Azure, MEGA, Filen, Internxt, kDrive, Jottacloud):
///   loads credentials from vault, uses ProviderFactory
/// - OAuth2 (Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho):
///   reads client_id/secret from connection_params, auto-refreshes tokens
/// - OAuth1 (4shared):
///   reads consumer key/secret from connection_params, loads access tokens from vault
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
        "filen" => create_via_factory(config, ProviderType::Filen).await,
        "internxt" => create_via_factory(config, ProviderType::Internxt).await,
        "kdrive" => create_via_factory(config, ProviderType::KDrive).await,
        "jottacloud" => create_via_factory(config, ProviderType::Jottacloud).await,

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

/// Load credentials from vault, build ProviderConfig, create via factory, connect
async fn create_via_factory(
    config: &CloudConfig,
    provider_type: ProviderType,
) -> Result<Box<dyn StorageProvider>, String> {
    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Credential vault not open".to_string())?;

    let creds_json = store
        .get(&format!("server_{}", config.server_profile))
        .map_err(|e| format!("No credentials for profile '{}': {}", config.server_profile, e))?;

    #[derive(serde::Deserialize)]
    struct SavedCreds {
        #[serde(default)]
        server: String,
        #[serde(default)]
        username: String,
        #[serde(default)]
        password: String,
    }

    let creds: SavedCreds = serde_json::from_str(&creds_json)
        .map_err(|e| format!("Failed to parse credentials: {}", e))?;

    // Merge connection_params into extra map
    let mut extra = std::collections::HashMap::new();
    if let Some(obj) = config.connection_params.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                extra.insert(k.clone(), s.to_string());
            } else if let Some(n) = v.as_u64() {
                extra.insert(k.clone(), n.to_string());
            } else if let Some(b) = v.as_bool() {
                extra.insert(k.clone(), b.to_string());
            }
        }
    }

    let port = extra
        .get("port")
        .and_then(|p| p.parse::<u16>().ok());

    let provider_config = ProviderConfig {
        name: config.cloud_name.clone(),
        provider_type,
        host: creds.server.clone(),
        port,
        username: Some(creds.username.clone()),
        password: Some(creds.password.clone()),
        initial_path: Some(config.remote_folder.clone()),
        extra,
    };

    let mut provider = ProviderFactory::create(&provider_config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;

    info!("AeroCloud: connecting via {:?} to {}", provider_type, creds.server);
    provider
        .connect()
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    Ok(provider)
}

// ── OAuth2 providers ───────────────────────────────────────────────

fn get_param<'a>(config: &'a CloudConfig, key: &str) -> Result<&'a str, String> {
    config
        .connection_params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing '{}' in connection_params", key))
}

async fn create_google_drive(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let client_id = get_param(config, "client_id")?;
    let client_secret = get_param(config, "client_secret")?;
    let gc = GoogleDriveConfig::new(client_id, client_secret);
    let mut p = GoogleDriveProvider::new(gc);
    p.connect().await.map_err(|e| format!("Google Drive: {}", e))?;
    info!("AeroCloud: connected to Google Drive");
    Ok(Box::new(p))
}

async fn create_dropbox(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let app_key = get_param(config, "client_id")?;
    let app_secret = get_param(config, "client_secret")?;
    let dc = DropboxConfig::new(app_key, app_secret);
    let mut p = DropboxProvider::new(dc);
    p.connect().await.map_err(|e| format!("Dropbox: {}", e))?;
    info!("AeroCloud: connected to Dropbox");
    Ok(Box::new(p))
}

async fn create_onedrive(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let client_id = get_param(config, "client_id")?;
    let client_secret = get_param(config, "client_secret")?;
    let oc = OneDriveConfig::new(client_id, client_secret);
    let mut p = OneDriveProvider::new(oc);
    p.connect().await.map_err(|e| format!("OneDrive: {}", e))?;
    info!("AeroCloud: connected to OneDrive");
    Ok(Box::new(p))
}

async fn create_box(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let client_id = get_param(config, "client_id")?;
    let client_secret = get_param(config, "client_secret")?;
    let bc = BoxConfig::new(client_id, client_secret);
    let mut p = BoxProvider::new(bc);
    p.connect().await.map_err(|e| format!("Box: {}", e))?;
    info!("AeroCloud: connected to Box");
    Ok(Box::new(p))
}

async fn create_pcloud(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let client_id = get_param(config, "client_id")?;
    let client_secret = get_param(config, "client_secret")?;
    let region = config.connection_params.get("region")
        .and_then(|v| v.as_str())
        .unwrap_or("us");
    let pc = PCloudConfig::new(client_id, client_secret, region);
    let mut p = PCloudProvider::new(pc);
    p.connect().await.map_err(|e| format!("pCloud: {}", e))?;
    info!("AeroCloud: connected to pCloud ({})", region);
    Ok(Box::new(p))
}

async fn create_zoho(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let client_id = get_param(config, "client_id")?;
    let client_secret = get_param(config, "client_secret")?;
    let region = config.connection_params.get("region")
        .and_then(|v| v.as_str())
        .unwrap_or("us");
    let zc = ZohoWorkdriveConfig::new(client_id, client_secret, region);
    let mut p = ZohoWorkdriveProvider::new(zc);
    p.connect().await.map_err(|e| format!("Zoho WorkDrive: {}", e))?;
    info!("AeroCloud: connected to Zoho WorkDrive ({})", region);
    Ok(Box::new(p))
}

// ── OAuth1 (4shared) ───────────────────────────────────────────────

async fn create_fourshared(config: &CloudConfig) -> Result<Box<dyn StorageProvider>, String> {
    let consumer_key = get_param(config, "consumer_key")?;
    let consumer_secret = get_param(config, "consumer_secret")?;

    // Load stored access tokens from vault
    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Credential vault not open".to_string())?;

    let token_json = store
        .get("fourshared_oauth_tokens")
        .map_err(|e| format!("No 4shared tokens in vault: {}", e))?;

    #[derive(serde::Deserialize)]
    struct FourSharedTokens {
        access_token: String,
        access_token_secret: String,
    }

    let tokens: FourSharedTokens = serde_json::from_str(&token_json)
        .map_err(|e| format!("Failed to parse 4shared tokens: {}", e))?;

    let fs_config = FourSharedConfig {
        consumer_key: consumer_key.to_string(),
        consumer_secret: SecretString::from(consumer_secret.to_string()),
        access_token: SecretString::from(tokens.access_token),
        access_token_secret: SecretString::from(tokens.access_token_secret),
    };

    let mut p = FourSharedProvider::new(fs_config);
    p.connect().await.map_err(|e| format!("4shared: {}", e))?;
    info!("AeroCloud: connected to 4shared");
    Ok(Box::new(p))
}
