//! Provider Commands - Tauri commands for multi-protocol cloud storage
//!
//! This module provides Tauri commands that route operations through
//! the StorageProvider abstraction, enabling support for FTP, WebDAV, S3, etc.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::providers::{
    StorageProvider, ProviderFactory, ProviderType,
    ProviderConfig, RemoteEntry,
};

/// State for managing the active storage provider
pub struct ProviderState {
    /// Currently active provider (if connected)
    pub provider: Mutex<Option<Box<dyn StorageProvider>>>,
    /// Current provider configuration
    pub config: Mutex<Option<ProviderConfig>>,
}

impl ProviderState {
    pub fn new() -> Self {
        Self {
            provider: Mutex::new(None),
            config: Mutex::new(None),
        }
    }
}

impl Default for ProviderState {
    fn default() -> Self {
        Self::new()
    }
}

// ============ Request/Response Types ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConnectionParams {
    /// Protocol type: "ftp", "ftps", "webdav", "s3"
    pub protocol: String,
    /// Host/URL (FTP server, WebDAV URL, or S3 endpoint)
    pub server: String,
    /// Port (optional, defaults based on protocol)
    pub port: Option<u16>,
    /// Username or Access Key ID
    pub username: String,
    /// Password or Secret Access Key
    pub password: String,
    /// Initial remote path to navigate to
    pub initial_path: Option<String>,
    /// S3 bucket name
    pub bucket: Option<String>,
    /// S3/cloud region
    pub region: Option<String>,
    /// Custom endpoint for S3-compatible services
    pub endpoint: Option<String>,
    /// Use path-style URLs for S3
    pub path_style: Option<bool>,
    /// Save session keys (MEGA)
    pub save_session: Option<bool>,
    /// Session expiry timestamp (MEGA)
    pub session_expires_at: Option<i64>,
}

impl ProviderConnectionParams {
    /// Convert to provider configuration
    pub fn to_provider_config(&self) -> Result<ProviderConfig, String> {
        let provider_type = match self.protocol.to_lowercase().as_str() {
            "ftp" => ProviderType::Ftp,
            "ftps" => ProviderType::Ftps,
            "sftp" => ProviderType::Sftp,
            "webdav" => ProviderType::WebDav,
            "s3" => ProviderType::S3,
            "mega" => ProviderType::Mega,
            other => return Err(format!("Unknown protocol: {}", other)),
        };

        let mut extra = std::collections::HashMap::new();
        
        // Add S3-specific options
        if provider_type == ProviderType::S3 {
            if let Some(ref bucket) = self.bucket {
                extra.insert("bucket".to_string(), bucket.clone());
            } else {
                return Err("S3 requires a bucket name".to_string());
            }
            if let Some(ref region) = self.region {
                extra.insert("region".to_string(), region.clone());
            } else {
                extra.insert("region".to_string(), "us-east-1".to_string());
            }
            if let Some(ref endpoint) = self.endpoint {
                extra.insert("endpoint".to_string(), endpoint.clone());
            }
            if self.path_style.unwrap_or(false) {
                extra.insert("path_style".to_string(), "true".to_string());
            }
        }

        // Add MEGA-specific options
        if provider_type == ProviderType::Mega {
            if self.save_session.unwrap_or(false) {
                extra.insert("save_session".to_string(), "true".to_string());
            }
            if let Some(ts) = self.session_expires_at {
                extra.insert("session_expires_at".to_string(), ts.to_string());
            }
        }

        let host = if provider_type == ProviderType::Mega {
            "mega.nz".to_string()
        } else {
            self.server.clone()
        };

        Ok(ProviderConfig {
            name: format!("{}@{}", self.username, host),
            provider_type,
            host,
            port: self.port,
            username: Some(self.username.clone()),
            password: Some(self.password.clone()),
            initial_path: self.initial_path.clone(),
            extra,
        })
    }
}

#[derive(Serialize)]
pub struct ProviderListResponse {
    pub files: Vec<RemoteEntry>,
    pub current_path: String,
}

#[derive(Serialize)]
pub struct ProviderConnectionInfo {
    pub connected: bool,
    pub protocol: Option<String>,
    pub display_name: Option<String>,
    pub server_info: Option<String>,
}

// ============ Tauri Commands ============

/// Connect to a storage provider using the specified protocol
#[tauri::command]
pub async fn provider_connect(
    state: State<'_, ProviderState>,
    params: ProviderConnectionParams,
) -> Result<String, String> {
    info!("Connecting to {} provider: {}", params.protocol, params.server);
    
    let config = params.to_provider_config()?;
    
    // Create provider using factory
    let mut provider = ProviderFactory::create(&config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;
    
    // Connect
    provider.connect().await
        .map_err(|e| format!("Connection failed: {}", e))?;
    
    let display_name = provider.display_name();
    let protocol = format!("{:?}", provider.provider_type());
    
    // Store provider and config
    {
        let mut prov_lock = state.provider.lock().await;
        *prov_lock = Some(provider);
    }
    {
        let mut config_lock = state.config.lock().await;
        *config_lock = Some(config);
    }
    
    info!("Connected successfully: {}", display_name);
    Ok(format!("Connected to {} via {}", display_name, protocol))
}

/// Disconnect from the current provider
#[tauri::command]
pub async fn provider_disconnect(
    state: State<'_, ProviderState>,
) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;
    
    if let Some(ref mut provider) = *provider_lock {
        info!("Disconnecting from provider: {}", provider.display_name());
        provider.disconnect().await
            .map_err(|e| format!("Disconnect failed: {}", e))?;
    }
    
    *provider_lock = None;
    
    let mut config_lock = state.config.lock().await;
    *config_lock = None;
    
    Ok(())
}

/// Check if connected to a provider
#[tauri::command]
pub async fn provider_check_connection(
    state: State<'_, ProviderState>,
) -> Result<ProviderConnectionInfo, String> {
    let provider_lock = state.provider.lock().await;
    
    match &*provider_lock {
        Some(provider) => Ok(ProviderConnectionInfo {
            connected: provider.is_connected(),
            protocol: Some(format!("{:?}", provider.provider_type())),
            display_name: Some(provider.display_name()),
            server_info: None,
        }),
        None => Ok(ProviderConnectionInfo {
            connected: false,
            protocol: None,
            display_name: None,
            server_info: None,
        }),
    }
}

/// List files in the specified path
#[tauri::command]
pub async fn provider_list_files(
    state: State<'_, ProviderState>,
    path: Option<String>,
) -> Result<ProviderListResponse, String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    let list_path = path.as_deref().unwrap_or(".");
    
    let files = provider.list(list_path).await
        .map_err(|e| format!("Failed to list files: {}", e))?;
    
    let current_path = provider.pwd().await
        .unwrap_or_else(|_| "/".to_string());
    
    Ok(ProviderListResponse {
        files,
        current_path,
    })
}

/// Change to the specified directory
#[tauri::command]
pub async fn provider_change_dir(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<ProviderListResponse, String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    // Handle ".." specifically to go to parent directory
    if path == ".." {
        provider.cd_up().await
            .map_err(|e| format!("Failed to go up: {}", e))?;
    } else {
        provider.cd(&path).await
            .map_err(|e| format!("Failed to change directory: {}", e))?;
    }
    
    let files = provider.list(".").await
        .map_err(|e| format!("Failed to list files: {}", e))?;
    
    let current_path = provider.pwd().await
        .unwrap_or_else(|_| path.clone());
    
    Ok(ProviderListResponse {
        files,
        current_path,
    })
}

/// Navigate to parent directory
#[tauri::command]
pub async fn provider_go_up(
    state: State<'_, ProviderState>,
) -> Result<ProviderListResponse, String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    provider.cd_up().await
        .map_err(|e| format!("Failed to go up: {}", e))?;
    
    let files = provider.list(".").await
        .map_err(|e| format!("Failed to list files: {}", e))?;
    
    let current_path = provider.pwd().await
        .unwrap_or_else(|_| "/".to_string());
    
    Ok(ProviderListResponse {
        files,
        current_path,
    })
}

/// Get current working directory
#[tauri::command]
pub async fn provider_pwd(
    state: State<'_, ProviderState>,
) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    provider.pwd().await
        .map_err(|e| format!("Failed to get working directory: {}", e))
}

/// Download a file from the remote server
#[tauri::command]
pub async fn provider_download_file(
    app: AppHandle,
    state: State<'_, ProviderState>,
    remote_path: String,
    local_path: String,
) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    let filename = std::path::Path::new(&remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    
    info!("Downloading via provider: {} -> {}", remote_path, local_path);
    
    // Create parent directory if needed
    if let Some(parent) = std::path::Path::new(&local_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    
    provider.download(&remote_path, &local_path, None).await
        .map_err(|e| format!("Download failed: {}", e))?;
    
    info!("Download completed: {}", filename);
    Ok(format!("Downloaded: {}", filename))
}

/// Download a folder recursively from the remote server (OAuth providers)
#[tauri::command]
pub async fn provider_download_folder(
    app: AppHandle,
    state: State<'_, ProviderState>,
    remote_path: String,
    local_path: String,
) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    let folder_name = std::path::Path::new(&remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".to_string());
    
    info!("Downloading folder via provider: {} -> {}", remote_path, local_path);
    
    // Create local folder
    tokio::fs::create_dir_all(&local_path).await
        .map_err(|e| format!("Failed to create local folder: {}", e))?;
    
    // Use a stack-based approach to download recursively
    let mut folders_to_scan: Vec<(String, String)> = vec![(remote_path.clone(), local_path.clone())];
    let mut files_downloaded = 0u32;
    
    while let Some((remote_folder, local_folder)) = folders_to_scan.pop() {
        // Change to the remote folder first
        provider.cd(&remote_folder).await
            .map_err(|e| format!("Failed to change to folder {}: {}", remote_folder, e))?;
        
        // List files in the current folder
        let files = provider.list(".").await
            .map_err(|e| format!("Failed to list files in {}: {}", remote_folder, e))?;
        
        for file in files {
            let remote_file_path = if remote_folder.ends_with('/') {
                format!("{}{}", remote_folder, file.name)
            } else {
                format!("{}/{}", remote_folder, file.name)
            };
            let local_file_path = format!("{}/{}", local_folder, file.name);
            
            if file.is_dir {
                // Create local subfolder and add to scan queue
                tokio::fs::create_dir_all(&local_file_path).await
                    .map_err(|e| format!("Failed to create folder {}: {}", local_file_path, e))?;
                folders_to_scan.push((remote_file_path, local_file_path));
            } else {
                // Download file
                if let Err(e) = provider.download(&remote_file_path, &local_file_path, None).await {
                    warn!("Failed to download {}: {}", remote_file_path, e);
                } else {
                    files_downloaded += 1;
                }
            }
        }
    }
    
    info!("Folder download completed: {} ({} files)", folder_name, files_downloaded);
    Ok(format!("Downloaded folder: {} ({} files)", folder_name, files_downloaded))
}

/// Upload a file to the remote server
#[tauri::command]
pub async fn provider_upload_file(
    app: AppHandle,
    state: State<'_, ProviderState>,
    local_path: String,
    remote_path: String,
) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    let filename = std::path::Path::new(&local_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    
    info!("Uploading via provider: {} -> {}", local_path, remote_path);
    
    provider.upload(&local_path, &remote_path, None).await
        .map_err(|e| format!("Upload failed: {}", e))?;
    
    info!("Upload completed: {}", filename);
    Ok(format!("Uploaded: {}", filename))
}

/// Create a directory
#[tauri::command]
pub async fn provider_mkdir(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    info!("Creating directory: {}", path);
    
    provider.mkdir(&path).await
        .map_err(|e| format!("Failed to create directory: {}", e))?;
    
    Ok(())
}

/// Delete a file
#[tauri::command]
pub async fn provider_delete_file(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    info!("Deleting file: {}", path);
    
    provider.delete(&path).await
        .map_err(|e| format!("Failed to delete file: {}", e))?;
    
    Ok(())
}

/// Delete a directory
#[tauri::command]
pub async fn provider_delete_dir(
    state: State<'_, ProviderState>,
    path: String,
    recursive: bool,
) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    info!("Deleting directory: {} (recursive: {})", path, recursive);
    
    if recursive {
        provider.rmdir_recursive(&path).await
            .map_err(|e| format!("Failed to delete directory: {}", e))?;
    } else {
        provider.rmdir(&path).await
            .map_err(|e| format!("Failed to delete directory: {}", e))?;
    }
    
    Ok(())
}

/// Rename a file or directory
#[tauri::command]
pub async fn provider_rename(
    state: State<'_, ProviderState>,
    from: String,
    to: String,
) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    info!("Renaming: {} -> {}", from, to);
    
    provider.rename(&from, &to).await
        .map_err(|e| format!("Failed to rename: {}", e))?;
    
    Ok(())
}

/// Get file/directory information
#[tauri::command]
pub async fn provider_stat(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<RemoteEntry, String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    provider.stat(&path).await
        .map_err(|e| format!("Failed to get file info: {}", e))
}

/// Keep connection alive (NOOP equivalent)
#[tauri::command]
pub async fn provider_keep_alive(
    state: State<'_, ProviderState>,
) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;
    
    if let Some(ref mut provider) = *provider_lock {
        provider.keep_alive().await
            .map_err(|e| format!("Keep alive failed: {}", e))?;
    }
    
    Ok(())
}

/// Get server information
#[tauri::command]
pub async fn provider_server_info(
    state: State<'_, ProviderState>,
) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    provider.server_info().await
        .map_err(|e| format!("Failed to get server info: {}", e))
}

/// Get file size
#[tauri::command]
pub async fn provider_file_size(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<u64, String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    provider.size(&path).await
        .map_err(|e| format!("Failed to get file size: {}", e))
}

/// Check if a file/directory exists
#[tauri::command]
pub async fn provider_exists(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<bool, String> {
    let mut provider_lock = state.provider.lock().await;
    
    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;
    
    provider.exists(&path).await
        .map_err(|e| format!("Failed to check existence: {}", e))
}

// ============ OAuth2 Commands ============

/// OAuth2 connection parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConnectionParams {
    /// Provider: "google_drive", "dropbox", "onedrive"
    pub provider: String,
    /// OAuth2 client ID (from app registration)
    pub client_id: String,
    /// OAuth2 client secret (from app registration)
    pub client_secret: String,
}

/// OAuth2 flow state
#[derive(Debug, Clone, Serialize)]
pub struct OAuthFlowStarted {
    /// URL to open in browser
    pub auth_url: String,
    /// State parameter for verification
    pub state: String,
}

/// Start OAuth2 authentication flow
/// Returns the authorization URL to open in browser
#[tauri::command]
pub async fn oauth2_start_auth(
    params: OAuthConnectionParams,
) -> Result<OAuthFlowStarted, String> {
    use crate::providers::{OAuth2Manager, OAuthConfig};
    
    info!("Starting OAuth2 flow for {}", params.provider);
    
    let config = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            OAuthConfig::google(&params.client_id, &params.client_secret)
        }
        "dropbox" => {
            OAuthConfig::dropbox(&params.client_id, &params.client_secret)
        }
        "onedrive" | "microsoft" => {
            OAuthConfig::onedrive(&params.client_id, &params.client_secret)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };
    
    let manager = OAuth2Manager::new();
    let (auth_url, state) = manager.start_auth_flow(&config).await
        .map_err(|e| format!("Failed to start OAuth flow: {}", e))?;
    
    // Open URL in default browser
    if let Err(e) = open::that(&auth_url) {
        info!("Could not open browser automatically: {}", e);
    }
    
    Ok(OAuthFlowStarted { auth_url, state })
}

/// Complete OAuth2 authentication with the authorization code
#[tauri::command]
pub async fn oauth2_complete_auth(
    params: OAuthConnectionParams,
    code: String,
    state: String,
) -> Result<String, String> {
    use crate::providers::{OAuth2Manager, OAuthConfig};
    
    info!("Completing OAuth2 flow for {}", params.provider);
    
    let config = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            OAuthConfig::google(&params.client_id, &params.client_secret)
        }
        "dropbox" => {
            OAuthConfig::dropbox(&params.client_id, &params.client_secret)
        }
        "onedrive" | "microsoft" => {
            OAuthConfig::onedrive(&params.client_id, &params.client_secret)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };
    
    let manager = OAuth2Manager::new();
    manager.complete_auth_flow(&config, &code, &state).await
        .map_err(|e| format!("Failed to complete OAuth flow: {}", e))?;
    
    Ok("Authentication successful".to_string())
}

/// Connect to an OAuth2-based cloud provider (after authentication)
#[tauri::command]
pub async fn oauth2_connect(
    state: State<'_, ProviderState>,
    params: OAuthConnectionParams,
) -> Result<String, String> {
    use crate::providers::{GoogleDriveProvider, DropboxProvider, OneDriveProvider, 
                          google_drive::GoogleDriveConfig, dropbox::DropboxConfig, 
                          onedrive::OneDriveConfig};
    
    info!("Connecting to OAuth2 provider: {}", params.provider);
    
    let provider: Box<dyn StorageProvider> = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            let config = GoogleDriveConfig::new(&params.client_id, &params.client_secret);
            let mut p = GoogleDriveProvider::new(config);
            p.connect().await
                .map_err(|e| format!("Google Drive connection failed: {}", e))?;
            Box::new(p)
        }
        "dropbox" => {
            let config = DropboxConfig::new(&params.client_id, &params.client_secret);
            let mut p = DropboxProvider::new(config);
            p.connect().await
                .map_err(|e| format!("Dropbox connection failed: {}", e))?;
            Box::new(p)
        }
        "onedrive" | "microsoft" => {
            let config = OneDriveConfig::new(&params.client_id, &params.client_secret);
            let mut p = OneDriveProvider::new(config);
            p.connect().await
                .map_err(|e| format!("OneDrive connection failed: {}", e))?;
            Box::new(p)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };
    
    let display_name = provider.display_name();
    
    // Store provider
    let mut provider_lock = state.provider.lock().await;
    *provider_lock = Some(provider);
    
    info!("Connected to {}", display_name);
    Ok(display_name)
}

/// Full OAuth2 authentication flow - starts server, opens browser, waits for callback, completes auth
#[tauri::command]
pub async fn oauth2_full_auth(
    params: OAuthConnectionParams,
) -> Result<String, String> {
    use crate::providers::{OAuth2Manager, OAuthConfig, oauth2::start_callback_server};
    
    info!("Starting full OAuth2 flow for {}", params.provider);
    
    let config = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            OAuthConfig::google(&params.client_id, &params.client_secret)
        }
        "dropbox" => {
            OAuthConfig::dropbox(&params.client_id, &params.client_secret)
        }
        "onedrive" | "microsoft" => {
            OAuthConfig::onedrive(&params.client_id, &params.client_secret)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };
    
    // Create manager ONCE and keep it for the entire flow
    let manager = OAuth2Manager::new();
    
    // Generate auth URL first
    let (auth_url, expected_state) = manager.start_auth_flow(&config).await
        .map_err(|e| format!("Failed to start OAuth flow: {}", e))?;
    
    // Start callback server in background BEFORE opening browser
    let callback_handle = tokio::spawn(async move {
        start_callback_server(17548).await
    });
    
    // Give the server a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    
    // Open URL in default browser
    if let Err(e) = open::that(&auth_url) {
        info!("Could not open browser automatically: {}", e);
        return Err(format!("Could not open browser: {}. Please open this URL manually: {}", e, auth_url));
    }
    
    info!("Browser opened, waiting for callback...");
    
    // Wait for callback (with timeout)
    let callback_result = tokio::time::timeout(
        tokio::time::Duration::from_secs(300), // 5 minute timeout
        callback_handle
    ).await
        .map_err(|_| "OAuth timeout: no response within 5 minutes")?
        .map_err(|e| format!("Callback server error: {}", e))?
        .map_err(|e| format!("Callback error: {}", e))?;
    
    let (code, state) = callback_result;
    
    // Verify state matches
    if state != expected_state {
        return Err("OAuth state mismatch - possible CSRF attack".to_string());
    }
    
    info!("Callback received, completing authentication...");
    
    // Complete the flow using the SAME manager instance (which has the PKCE verifier stored)
    manager.complete_auth_flow(&config, &code, &expected_state).await
        .map_err(|e| format!("Failed to exchange code for tokens: {}", e))?;
    
    info!("OAuth2 authentication completed successfully for {}", params.provider);
    Ok("Authentication successful! You can now connect.".to_string())
}

/// Check if OAuth2 tokens exist for a provider
#[tauri::command]
pub async fn oauth2_has_tokens(
    provider: String,
) -> Result<bool, String> {
    use crate::providers::{OAuth2Manager, OAuthProvider};
    
    let oauth_provider = match provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => OAuthProvider::Google,
        "dropbox" => OAuthProvider::Dropbox,
        "onedrive" | "microsoft" => OAuthProvider::OneDrive,
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };
    
    let manager = OAuth2Manager::new();
    Ok(manager.has_tokens(oauth_provider))
}

/// Clear OAuth2 tokens for a provider (logout)
#[tauri::command]
pub async fn oauth2_logout(
    provider: String,
) -> Result<(), String> {
    use crate::providers::{OAuth2Manager, OAuthProvider};
    
    let oauth_provider = match provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => OAuthProvider::Google,
        "dropbox" => OAuthProvider::Dropbox,
        "onedrive" | "microsoft" => OAuthProvider::OneDrive,
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };
    
    let manager = OAuth2Manager::new();
    manager.clear_tokens(oauth_provider)
        .map_err(|e| format!("Failed to clear tokens: {}", e))?;
    
    info!("Logged out from {}", provider);
    Ok(())
}

/// Create a shareable link for a file using the OAuth provider's native sharing API
/// Works with Google Drive, Dropbox, and OneDrive
#[tauri::command]
pub async fn provider_create_share_link(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    
    if !provider.supports_share_links() {
        return Err(format!(
            "{} does not support native share links", 
            provider.provider_type()
        ));
    }
    
    let share_url = provider.create_share_link(&path, None).await
        .map_err(|e| format!("Failed to create share link: {}", e))?;
    
    info!("Created share link for {}: {}", path, share_url);
    Ok(share_url)
}
