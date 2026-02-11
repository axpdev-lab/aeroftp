//! Provider Commands - Tauri commands for multi-protocol cloud storage
//!
//! This module provides Tauri commands that route operations through
//! the StorageProvider abstraction, enabling support for FTP, WebDAV, S3, etc.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::providers::{
    StorageProvider, ProviderFactory, ProviderType,
    ProviderConfig, RemoteEntry, StorageInfo,
    FileVersion, LockInfo, SharePermission,
};

/// State for managing the active storage provider
pub struct ProviderState {
    /// Currently active provider (if connected)
    pub provider: Mutex<Option<Box<dyn StorageProvider>>>,
    /// Current provider configuration
    pub config: Mutex<Option<ProviderConfig>>,
    /// Cancel flag for aborting folder transfers
    pub cancel_flag: Mutex<bool>,
}

impl ProviderState {
    pub fn new() -> Self {
        Self {
            provider: Mutex::new(None),
            config: Mutex::new(None),
            cancel_flag: Mutex::new(false),
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
    /// Protocol type: "ftp", "ftps", "sftp", "webdav", "s3", "mega"
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
    /// SFTP: Path to private key file
    pub private_key_path: Option<String>,
    /// SFTP: Passphrase for encrypted private key
    pub key_passphrase: Option<String>,
    /// SFTP: Connection timeout in seconds
    pub timeout: Option<u64>,
    /// FTP/FTPS: TLS mode ("none", "explicit", "implicit", "explicit_if_available")
    pub tls_mode: Option<String>,
    /// FTP/FTPS: Accept invalid/self-signed certificates
    pub verify_cert: Option<bool>,
    /// Filen: Optional TOTP 2FA code
    pub two_factor_code: Option<String>,
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
            "box" => ProviderType::Box,
            "pcloud" => ProviderType::PCloud,
            "azure" => ProviderType::Azure,
            "filen" => ProviderType::Filen,
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

        // Add FTP/FTPS-specific options
        if provider_type == ProviderType::Ftp || provider_type == ProviderType::Ftps {
            if let Some(ref tls_mode) = self.tls_mode {
                extra.insert("tls_mode".to_string(), tls_mode.clone());
            }
            if let Some(verify) = self.verify_cert {
                extra.insert("verify_cert".to_string(), verify.to_string());
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

        // Add Azure-specific options
        if provider_type == ProviderType::Azure {
            if let Some(ref bucket) = self.bucket {
                extra.insert("container".to_string(), bucket.clone());
            }
            if let Some(ref endpoint) = self.endpoint {
                extra.insert("endpoint".to_string(), endpoint.clone());
            }
            // account_name comes from username field
        }

        // Add Filen-specific options
        if provider_type == ProviderType::Filen {
            if let Some(ref code) = self.two_factor_code {
                if !code.is_empty() {
                    extra.insert("two_factor_code".to_string(), code.clone());
                }
            }
        }

        // Add pCloud-specific options
        if provider_type == ProviderType::PCloud {
            if let Some(ref region) = self.region {
                extra.insert("region".to_string(), region.clone());
            } else {
                extra.insert("region".to_string(), "us".to_string());
            }
        }

        // Add SFTP-specific options
        if provider_type == ProviderType::Sftp {
            if let Some(ref key_path) = self.private_key_path {
                if !key_path.is_empty() {
                    extra.insert("private_key_path".to_string(), key_path.clone());
                }
            }
            if let Some(ref passphrase) = self.key_passphrase {
                if !passphrase.is_empty() {
                    extra.insert("key_passphrase".to_string(), passphrase.clone());
                }
            }
            if let Some(timeout) = self.timeout {
                extra.insert("timeout".to_string(), timeout.to_string());
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
    _app: AppHandle,
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
    #[allow(unused_variables)]
    file_exists_action: Option<String>,
) -> Result<String, String> {
    let file_exists_action = file_exists_action.unwrap_or_default();

    // Reset cancel flag
    {
        let mut cancel = state.cancel_flag.lock().await;
        *cancel = false;
    }

    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;

    let folder_name = std::path::Path::new(&remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".to_string());

    let transfer_id = format!("dl-folder-{}", chrono::Utc::now().timestamp_millis());

    info!("Downloading folder via provider: {} -> {}", remote_path, local_path);

    // Emit start event
    let _ = app.emit("transfer_event", crate::TransferEvent {
        event_type: "start".to_string(),
        transfer_id: transfer_id.clone(),
        filename: folder_name.clone(),
        direction: "download".to_string(),
        message: Some(format!("Starting folder download: {}", folder_name)),
        progress: None,
        path: Some(remote_path.clone()),
    });

    // Create local folder
    tokio::fs::create_dir_all(&local_path).await
        .map_err(|e| format!("Failed to create local folder: {}", e))?;

    // Use a stack-based approach to download recursively
    let mut folders_to_scan: Vec<(String, String)> = vec![(remote_path.clone(), local_path.clone())];
    let mut files_downloaded = 0u32;
    let mut files_skipped = 0u32;
    let mut files_errored = 0u32;
    let mut file_index = 0u32;

    while let Some((remote_folder, local_folder)) = folders_to_scan.pop() {
        // Change to the remote folder first
        provider.cd(&remote_folder).await
            .map_err(|e| format!("Failed to change to folder {}: {}", remote_folder, e))?;

        // List files in the current folder
        let files = provider.list(".").await
            .map_err(|e| format!("Failed to list files in {}: {}", remote_folder, e))?;

        for file in files {
            // Check cancel flag before each file
            {
                let cancel = state.cancel_flag.lock().await;
                if *cancel {
                    info!("Provider folder download cancelled by user after {} files", files_downloaded);
                    let _ = app.emit("transfer_event", crate::TransferEvent {
                        event_type: "cancelled".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: folder_name.clone(),
                        direction: "download".to_string(),
                        message: Some(format!("Download cancelled after {} files", files_downloaded)),
                        progress: None,
                        path: None,
                    });
                    return Ok(format!("Download cancelled after {} files", files_downloaded));
                }
            }

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
                // Check if local file exists and should be skipped
                if !file_exists_action.is_empty() && file_exists_action != "overwrite" {
                    let local_p = std::path::Path::new(&local_file_path);
                    if let Ok(local_meta) = std::fs::metadata(local_p) {
                        if local_meta.is_file() {
                            let remote_modified = file.modified.as_ref().and_then(|s| {
                                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                                    .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S"))
                                    .ok()
                                    .map(|ndt| ndt.and_utc())
                            });
                            if crate::should_skip_file_download(&file_exists_action, remote_modified, file.size, &local_meta) {
                                files_skipped += 1;
                                let _ = app.emit("transfer_event", crate::TransferEvent {
                                    event_type: "file_skip".to_string(),
                                    transfer_id: format!("{}-{}", transfer_id, file_index),
                                    filename: file.name.clone(),
                                    direction: "download".to_string(),
                                    message: Some(format!("Skipped (identical): {}", file.name)),
                                    progress: None,
                                    path: Some(remote_file_path.clone()),
                                });
                                file_index += 1;
                                continue;
                            }
                        }
                    }
                }

                let file_transfer_id = format!("{}-{}", transfer_id, file_index);

                // Emit file_start event
                let _ = app.emit("transfer_event", crate::TransferEvent {
                    event_type: "file_start".to_string(),
                    transfer_id: file_transfer_id.clone(),
                    filename: file.name.clone(),
                    direction: "download".to_string(),
                    message: Some(format!("Downloading: {}", remote_file_path)),
                    progress: Some(crate::TransferProgress {
                        transfer_id: file_transfer_id.clone(),
                        filename: file.name.clone(),
                        transferred: 0,
                        total: file.size,
                        percentage: 0,
                        speed_bps: 0,
                        eta_seconds: 0,
                        direction: "download".to_string(),
                        total_files: None,
                        path: None,
                    }),
                    path: Some(remote_file_path.clone()),
                });

                // Download file
                if let Err(e) = provider.download(&remote_file_path, &local_file_path, None).await {
                    warn!("Failed to download {}: {}", remote_file_path, e);
                    files_errored += 1;
                    let _ = app.emit("transfer_event", crate::TransferEvent {
                        event_type: "file_error".to_string(),
                        transfer_id: file_transfer_id,
                        filename: file.name.clone(),
                        direction: "download".to_string(),
                        message: Some(format!("Failed: {}", e)),
                        progress: None,
                        path: Some(remote_file_path.clone()),
                    });
                } else {
                    files_downloaded += 1;
                    let _ = app.emit("transfer_event", crate::TransferEvent {
                        event_type: "file_complete".to_string(),
                        transfer_id: file_transfer_id,
                        filename: file.name.clone(),
                        direction: "download".to_string(),
                        message: Some(format!("Downloaded: {} ({}/{})", file.name, files_downloaded, files_downloaded + files_skipped)),
                        progress: None,
                        path: Some(remote_file_path.clone()),
                    });
                }

                file_index += 1;
            }
        }
    }

    // Emit complete event
    let _ = app.emit("transfer_event", crate::TransferEvent {
        event_type: "complete".to_string(),
        transfer_id: transfer_id,
        filename: folder_name.clone(),
        direction: "download".to_string(),
        message: Some(format!("Downloaded {} files, {} skipped, {} errors", files_downloaded, files_skipped, files_errored)),
        progress: None,
        path: None,
    });

    info!("Folder download completed: {} ({} files)", folder_name, files_downloaded);
    Ok(format!("Downloaded folder: {} ({} files)", folder_name, files_downloaded))
}

/// Upload a file to the remote server
#[tauri::command]
pub async fn provider_upload_file(
    _app: AppHandle,
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
        "box" => {
            OAuthConfig::box_cloud(&params.client_id, &params.client_secret)
        }
        "pcloud" => {
            OAuthConfig::pcloud(&params.client_id, &params.client_secret)
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
        "box" => {
            OAuthConfig::box_cloud(&params.client_id, &params.client_secret)
        }
        "pcloud" => {
            OAuthConfig::pcloud(&params.client_id, &params.client_secret)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };

    let manager = OAuth2Manager::new();
    manager.complete_auth_flow(&config, &code, &state).await
        .map_err(|e| format!("Failed to complete OAuth flow: {}", e))?;
    
    Ok("Authentication successful".to_string())
}

/// OAuth2 connection result with display name and account email
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2ConnectResult {
    pub display_name: String,
    pub account_email: Option<String>,
}

/// Connect to an OAuth2-based cloud provider (after authentication)
#[tauri::command]
pub async fn oauth2_connect(
    state: State<'_, ProviderState>,
    params: OAuthConnectionParams,
) -> Result<OAuth2ConnectResult, String> {
    use crate::providers::{GoogleDriveProvider, DropboxProvider, OneDriveProvider, BoxProvider, PCloudProvider,
                          google_drive::GoogleDriveConfig, dropbox::DropboxConfig,
                          onedrive::OneDriveConfig, types::BoxConfig, types::PCloudConfig};

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
        "box" => {
            let config = BoxConfig {
                client_id: params.client_id.clone(),
                client_secret: params.client_secret.clone(),
            };
            let mut p = BoxProvider::new(config);
            p.connect().await
                .map_err(|e| format!("Box connection failed: {}", e))?;
            Box::new(p)
        }
        "pcloud" => {
            let config = PCloudConfig {
                client_id: params.client_id.clone(),
                client_secret: params.client_secret.clone(),
                region: "us".to_string(),
            };
            let mut p = PCloudProvider::new(config);
            p.connect().await
                .map_err(|e| format!("pCloud connection failed: {}", e))?;
            Box::new(p)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };
    
    let display_name = provider.display_name();
    let account_email = provider.account_email();

    // Store provider
    let mut provider_lock = state.provider.lock().await;
    *provider_lock = Some(provider);

    info!("Connected to {} ({})", display_name, account_email.as_deref().unwrap_or("no email"));
    Ok(OAuth2ConnectResult { display_name, account_email })
}

/// Full OAuth2 authentication flow - starts server, opens browser, waits for callback, completes auth
#[tauri::command]
pub async fn oauth2_full_auth(
    params: OAuthConnectionParams,
) -> Result<String, String> {
    use crate::providers::{OAuth2Manager, OAuthConfig, oauth2::{bind_callback_listener, bind_callback_listener_on_port, wait_for_callback}};

    info!("Starting full OAuth2 flow for {}", params.provider);

    // Some providers require exact redirect_uri matching, so use a fixed port
    let fixed_port: u16 = match params.provider.to_lowercase().as_str() {
        "box" => 9484,
        "dropbox" => 17548,
        _ => 0,
    };

    // Bind callback listener (fixed port for Box, ephemeral for others)
    let (listener, port) = if fixed_port > 0 {
        bind_callback_listener_on_port(fixed_port).await
    } else {
        bind_callback_listener().await
    }.map_err(|e| format!("Failed to bind callback listener: {}", e))?;

    let config = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            OAuthConfig::google_with_port(&params.client_id, &params.client_secret, port)
        }
        "dropbox" => {
            OAuthConfig::dropbox_with_port(&params.client_id, &params.client_secret, port)
        }
        "onedrive" | "microsoft" => {
            OAuthConfig::onedrive_with_port(&params.client_id, &params.client_secret, port)
        }
        "box" => {
            OAuthConfig::box_cloud_with_port(&params.client_id, &params.client_secret, port)
        }
        "pcloud" => {
            OAuthConfig::pcloud_with_port(&params.client_id, &params.client_secret, port)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };

    // Create manager ONCE and keep it for the entire flow
    let manager = OAuth2Manager::new();

    // Generate auth URL with the dynamic port in redirect_uri
    let (auth_url, expected_state) = manager.start_auth_flow(&config).await
        .map_err(|e| format!("Failed to start OAuth flow: {}", e))?;

    // Start waiting for callback in background (listener already bound)
    let callback_handle = tokio::spawn(async move {
        wait_for_callback(listener).await
    });
    
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
        "box" => OAuthProvider::Box,
        "pcloud" => OAuthProvider::PCloud,
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
        "box" => OAuthProvider::Box,
        "pcloud" => OAuthProvider::PCloud,
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

/// Remove a share/export link for a file or folder
#[tauri::command]
pub async fn provider_remove_share_link(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider.remove_share_link(&path).await
        .map_err(|e| format!("Failed to remove share link: {}", e))?;

    info!("Removed share link for {}", path);
    Ok(())
}

/// Import a file/folder from a public link into the account
#[tauri::command]
pub async fn provider_import_link(
    state: State<'_, ProviderState>,
    link: String,
    dest: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !provider.supports_import_link() {
        return Err(format!(
            "{} does not support importing from links",
            provider.provider_type()
        ));
    }

    provider.import_link(&link, &dest).await
        .map_err(|e| format!("Failed to import link: {}", e))?;

    info!("Imported link to {}", dest);
    Ok(())
}

/// Get storage quota information (used/total/free bytes)
#[tauri::command]
pub async fn provider_storage_info(
    state: State<'_, ProviderState>,
) -> Result<StorageInfo, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider.storage_info().await
        .map_err(|e| format!("Failed to get storage info: {}", e))
}

/// Get disk usage for a path in bytes
#[tauri::command]
pub async fn provider_disk_usage(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<u64, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider.disk_usage(&path).await
        .map_err(|e| format!("Failed to get disk usage: {}", e))
}

/// Search for files matching a pattern under the given path
#[tauri::command]
pub async fn provider_find(
    state: State<'_, ProviderState>,
    path: String,
    pattern: String,
) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !provider.supports_find() {
        return Err(format!(
            "{} does not support remote search",
            provider.provider_type()
        ));
    }

    provider.find(&path, &pattern).await
        .map_err(|e| format!("Search failed: {}", e))
}

/// Set transfer speed limits (KB/s, 0 = unlimited)
#[tauri::command]
pub async fn provider_set_speed_limit(
    state: State<'_, ProviderState>,
    upload_kb: u64,
    download_kb: u64,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider.set_speed_limit(upload_kb, download_kb).await
        .map_err(|e| format!("Failed to set speed limit: {}", e))
}

/// Get current transfer speed limits (upload_kb, download_kb) in KB/s
#[tauri::command]
pub async fn provider_get_speed_limit(
    state: State<'_, ProviderState>,
) -> Result<(u64, u64), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider.get_speed_limit().await
        .map_err(|e| format!("Failed to get speed limit: {}", e))
}

/// Check if the current provider supports resume transfers
#[tauri::command]
pub async fn provider_supports_resume(
    state: State<'_, ProviderState>,
) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_resume())
}

/// Resume a download from a given byte offset
#[tauri::command]
pub async fn provider_resume_download(
    state: State<'_, ProviderState>,
    remote_path: String,
    local_path: String,
    offset: u64,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if let Some(parent) = std::path::Path::new(&local_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    provider.resume_download(&remote_path, &local_path, offset, None).await
        .map_err(|e| format!("Resume download failed: {}", e))?;

    Ok(format!("Resume download completed: {}", remote_path))
}

/// Resume an upload from a given byte offset
#[tauri::command]
pub async fn provider_resume_upload(
    state: State<'_, ProviderState>,
    local_path: String,
    remote_path: String,
    offset: u64,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider.resume_upload(&local_path, &remote_path, offset, None).await
        .map_err(|e| format!("Resume upload failed: {}", e))?;

    Ok(format!("Resume upload completed: {}", remote_path))
}

// --- File Versions ---

#[tauri::command]
pub async fn provider_supports_versions(
    state: State<'_, ProviderState>,
) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_versions())
}

#[tauri::command]
pub async fn provider_list_versions(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<FileVersion>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider.list_versions(&path).await
        .map_err(|e| format!("List versions failed: {}", e))
}

#[tauri::command]
pub async fn provider_download_version(
    state: State<'_, ProviderState>,
    path: String,
    version_id: String,
    local_path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider.download_version(&path, &version_id, &local_path).await
        .map_err(|e| format!("Download version failed: {}", e))?;
    Ok(format!("Downloaded version {} of {}", version_id, path))
}

#[tauri::command]
pub async fn provider_restore_version(
    state: State<'_, ProviderState>,
    path: String,
    version_id: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider.restore_version(&path, &version_id).await
        .map_err(|e| format!("Restore version failed: {}", e))
}

// --- File Locking ---

#[tauri::command]
pub async fn provider_supports_locking(
    state: State<'_, ProviderState>,
) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_locking())
}

#[tauri::command]
pub async fn provider_lock_file(
    state: State<'_, ProviderState>,
    path: String,
    timeout: u64,
) -> Result<LockInfo, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider.lock_file(&path, timeout).await
        .map_err(|e| format!("Lock failed: {}", e))
}

#[tauri::command]
pub async fn provider_unlock_file(
    state: State<'_, ProviderState>,
    path: String,
    lock_token: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider.unlock_file(&path, &lock_token).await
        .map_err(|e| format!("Unlock failed: {}", e))
}

// --- Thumbnails ---

#[tauri::command]
pub async fn provider_supports_thumbnails(
    state: State<'_, ProviderState>,
) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_thumbnails())
}

#[tauri::command]
pub async fn provider_get_thumbnail(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider.get_thumbnail(&path).await
        .map_err(|e| format!("Get thumbnail failed: {}", e))
}

// --- Permissions / Advanced Sharing ---

#[tauri::command]
pub async fn provider_supports_permissions(
    state: State<'_, ProviderState>,
) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_permissions())
}

#[tauri::command]
pub async fn provider_list_permissions(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<SharePermission>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider.list_permissions(&path).await
        .map_err(|e| format!("List permissions failed: {}", e))
}

#[tauri::command]
pub async fn provider_add_permission(
    state: State<'_, ProviderState>,
    path: String,
    role: String,
    target_type: String,
    target: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    let perm = SharePermission { role, target_type, target };
    provider.add_permission(&path, &perm).await
        .map_err(|e| format!("Add permission failed: {}", e))
}

#[tauri::command]
pub async fn provider_remove_permission(
    state: State<'_, ProviderState>,
    path: String,
    target: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard.as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider.remove_permission(&path, &target).await
        .map_err(|e| format!("Remove permission failed: {}", e))
}

/// Compare local and remote directories using the StorageProvider trait.
/// Works with all protocols (SFTP, WebDAV, S3, Google Drive, etc.)
#[tauri::command]
pub async fn provider_compare_directories(
    app: AppHandle,
    state: State<'_, ProviderState>,
    local_path: String,
    remote_path: String,
    options: Option<crate::sync::CompareOptions>,
) -> Result<Vec<crate::sync::FileComparison>, String> {
    use std::collections::HashMap;
    use crate::sync::{FileInfo, should_exclude, build_comparison_results_with_index, load_sync_index};

    let options = options.unwrap_or_default();

    info!("Provider compare: local={}, remote={}", local_path, remote_path);

    let _ = app.emit("sync_scan_progress", serde_json::json!({
        "phase": "local", "files_found": 0,
    }));

    // Get local files (reuse the same logic from lib.rs)
    let local_files = crate::get_local_files_recursive(&local_path, &local_path, &options.exclude_patterns)
        .await
        .map_err(|e| format!("Failed to scan local directory: {}", e))?;

    let _ = app.emit("sync_scan_progress", serde_json::json!({
        "phase": "remote", "files_found": local_files.len(),
    }));

    // Get remote files via provider - lock/unlock per directory to avoid blocking other operations
    let mut remote_files: HashMap<String, FileInfo> = HashMap::new();
    let mut dirs_to_process = vec![remote_path.clone()];

    // First check we're connected
    {
        let provider_lock = state.provider.lock().await;
        if provider_lock.is_none() {
            return Err("Not connected to any provider".to_string());
        }
    }

    while let Some(current_dir) = dirs_to_process.pop() {
        // Lock provider only for this single list operation, then release
        let entries = {
            let mut provider_lock = state.provider.lock().await;
            let provider = provider_lock.as_mut()
                .ok_or("Not connected to any provider")?;
            provider.list(&current_dir).await
                .map_err(|e| format!("Failed to list {}: {}", current_dir, e))?
        };

        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }

            let relative_path = if current_dir == remote_path {
                entry.name.clone()
            } else {
                let rel_dir = current_dir.strip_prefix(&remote_path).unwrap_or(&current_dir);
                let rel_dir = rel_dir.trim_start_matches('/');
                if rel_dir.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", rel_dir, entry.name)
                }
            };

            if should_exclude(&relative_path, &options.exclude_patterns) {
                continue;
            }

            let modified = entry.modified.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok()
                    .or_else(|| {
                        chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M")
                            .ok()
                            .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc))
                    })
                    .or_else(|| {
                        chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S")
                            .ok()
                            .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc))
                    })
            });

            let file_info = FileInfo {
                name: entry.name.clone(),
                path: entry.path.clone(),
                size: entry.size,
                modified,
                is_dir: entry.is_dir,
                checksum: None,
            };

            remote_files.insert(relative_path, file_info);

            if entry.is_dir {
                let sub_path = if current_dir.ends_with('/') {
                    format!("{}{}", current_dir, entry.name)
                } else {
                    format!("{}/{}", current_dir, entry.name)
                };
                dirs_to_process.push(sub_path);
            }
        }

        let _ = app.emit("sync_scan_progress", serde_json::json!({
            "phase": "remote",
            "files_found": local_files.len() + remote_files.len(),
        }));
    }

    let _ = app.emit("sync_scan_progress", serde_json::json!({
        "phase": "comparing",
        "files_found": local_files.len() + remote_files.len(),
    }));

    let index = load_sync_index(&local_path, &remote_path).ok().flatten();
    let results = build_comparison_results_with_index(local_files, remote_files, &options, index.as_ref());
    info!("Provider compare complete: {} differences found (index: {})", results.len(), if index.is_some() { "used" } else { "none" });

    Ok(results)
}

// ============ 4shared OAuth 1.0 Commands ============

/// Parameters for 4shared OAuth 1.0 authentication
#[derive(Debug, Clone, Deserialize)]
pub struct FourSharedAuthParams {
    pub consumer_key: String,
    pub consumer_secret: String,
}

/// Result from starting 4shared OAuth flow
#[derive(Debug, Clone, Serialize)]
pub struct FourSharedAuthStarted {
    pub auth_url: String,
    pub request_token: String,
    pub request_token_secret: String,
}

/// Vault key for 4shared OAuth tokens
const FOURSHARED_TOKEN_KEY: &str = "oauth_fourshared";

/// Store 4shared tokens in credential vault (same pattern as OAuth2)
fn store_fourshared_tokens(access_token: &str, access_token_secret: &str) -> Result<(), String> {
    let token_data = format!("{}:{}", access_token, access_token_secret);

    // Try vault first
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        store.store(FOURSHARED_TOKEN_KEY, &token_data)
            .map_err(|e| format!("Failed to store tokens: {}", e))?;
        return Ok(());
    }

    // Try auto-init vault
    if crate::credential_store::CredentialStore::init().is_ok() {
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            store.store(FOURSHARED_TOKEN_KEY, &token_data)
                .map_err(|e| format!("Failed to store tokens: {}", e))?;
            return Ok(());
        }
    }

    Err("Credential vault not available. Please unlock the vault first.".to_string())
}

/// Load 4shared tokens from credential vault
fn load_fourshared_tokens() -> Result<(String, String), String> {
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        if let Ok(data) = store.get(FOURSHARED_TOKEN_KEY) {
            let parts: Vec<&str> = data.splitn(2, ':').collect();
            if parts.len() == 2 {
                return Ok((parts[0].to_string(), parts[1].to_string()));
            }
        }
    }
    Err("No 4shared tokens found. Please authenticate first.".to_string())
}

/// Start 4shared OAuth 1.0 flow — obtain request token, return auth URL
#[tauri::command]
pub async fn fourshared_start_auth(
    params: FourSharedAuthParams,
) -> Result<FourSharedAuthStarted, String> {
    use crate::providers::oauth1;

    info!("Starting 4shared OAuth 1.0 flow");

    // Bind a local callback listener to get a port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await
        .map_err(|e| format!("Failed to bind callback listener: {}", e))?;
    let port = listener.local_addr()
        .map_err(|e| format!("Failed to get listener port: {}", e))?.port();
    drop(listener);

    let callback_url = format!("http://127.0.0.1:{}/callback", port);

    let (request_token, request_token_secret) = oauth1::request_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/initiate",
        &callback_url,
    ).await?;

    let auth_url = oauth1::authorize_url(
        "https://api.4shared.com/v1_2/oauth/authorize",
        &request_token,
    );

    if let Err(e) = open::that(&auth_url) {
        info!("Could not open browser: {}", e);
    }

    Ok(FourSharedAuthStarted {
        auth_url,
        request_token,
        request_token_secret,
    })
}

/// Complete 4shared OAuth 1.0 flow — exchange request token + verifier for access token
#[tauri::command]
pub async fn fourshared_complete_auth(
    params: FourSharedAuthParams,
    request_token: String,
    request_token_secret: String,
    verifier: String,
) -> Result<String, String> {
    use crate::providers::oauth1;

    info!("Completing 4shared OAuth 1.0 flow");

    let (access_token, access_token_secret) = oauth1::access_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/token",
        &request_token,
        &request_token_secret,
        &verifier,
    ).await?;

    store_fourshared_tokens(&access_token, &access_token_secret)?;

    info!("4shared OAuth 1.0 authentication completed successfully");
    Ok("Authentication successful".to_string())
}

/// Full 4shared OAuth 1.0 flow — start server, open browser, wait for callback, exchange tokens
#[tauri::command]
pub async fn fourshared_full_auth(
    params: FourSharedAuthParams,
) -> Result<String, String> {
    use crate::providers::oauth1;

    info!("Starting full 4shared OAuth 1.0 flow");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await
        .map_err(|e| format!("Failed to bind callback listener: {}", e))?;
    let port = listener.local_addr()
        .map_err(|e| format!("Failed to get listener port: {}", e))?.port();

    let callback_url = format!("http://127.0.0.1:{}/callback", port);

    // Step 1: Request token
    let (request_token, request_token_secret) = oauth1::request_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/initiate",
        &callback_url,
    ).await?;

    // Step 2: Open authorization URL
    let auth_url = oauth1::authorize_url(
        "https://api.4shared.com/v1_2/oauth/authorize",
        &request_token,
    );

    if let Err(e) = open::that(&auth_url) {
        return Err(format!("Could not open browser: {}. Open manually: {}", e, auth_url));
    }

    info!("Browser opened, waiting for OAuth 1.0 callback on port {}...", port);

    // Step 3: Wait for callback
    let (token, verifier) = tokio::time::timeout(
        tokio::time::Duration::from_secs(300),
        wait_for_oauth1_callback(listener),
    )
    .await
    .map_err(|_| "OAuth timeout: no response within 5 minutes".to_string())?
    .map_err(|e| format!("Callback error: {}", e))?;

    if token != request_token {
        return Err("OAuth token mismatch — possible CSRF attack".to_string());
    }

    // Step 4: Exchange for access token
    let (access_token, access_token_secret) = oauth1::access_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/token",
        &request_token,
        &request_token_secret,
        &verifier,
    ).await?;

    store_fourshared_tokens(&access_token, &access_token_secret)?;

    info!("4shared OAuth 1.0 full auth completed successfully");
    Ok("Authentication successful! You can now connect.".to_string())
}

/// Wait for OAuth 1.0 callback (returns oauth_token, oauth_verifier).
/// oauth_verifier is optional — 4shared uses OAuth 1.0 (not 1.0a) and does NOT send a verifier.
/// Accepts connections in a loop to handle browser prefetch/favicon requests.
async fn wait_for_oauth1_callback(
    listener: tokio::net::TcpListener,
) -> Result<(String, String), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Accept connections in a loop — browsers may send favicon or prefetch requests first
    loop {
        let (mut stream, _) = listener.accept().await
            .map_err(|e| format!("Accept error: {}", e))?;

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await
            .map_err(|e| format!("Read error: {}", e))?;

        let request = String::from_utf8_lossy(&buf[..n]);

        // Parse the request line: GET /callback?oauth_token=xxx HTTP/1.1
        let request_path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("");

        // Ignore non-callback requests (favicon, prefetch, etc.)
        if !request_path.starts_with("/callback") {
            let response_404 = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
            let _ = stream.write_all(response_404.as_bytes()).await;
            let _ = stream.shutdown().await;
            continue;
        }

        let query = request_path.split('?').nth(1).unwrap_or("");

        let params: std::collections::HashMap<&str, &str> = query
            .split('&')
            .filter_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                Some((parts.next()?, parts.next()?))
            })
            .collect();

        let oauth_token = params.get("oauth_token")
            .ok_or("Missing oauth_token in callback")?
            .to_string();
        // oauth_verifier is optional — 4shared (OAuth 1.0, not 1.0a) doesn't send it
        let oauth_verifier = params.get("oauth_verifier")
            .map(|v| v.to_string())
            .unwrap_or_default();

        let response = r#"HTTP/1.1 200 OK
Content-Type: text/html; charset=utf-8
Connection: close

<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>AeroFTP - Authorization Complete</title>
    <style>
        * { margin: 0; padding: 0; box-sizing: border-box; }
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif;
            min-height: 100vh;
            display: flex;
            justify-content: center;
            align-items: center;
            background: linear-gradient(135deg, #0f0f1a 0%, #1a1a2e 50%, #16213e 100%);
            color: #fff;
            overflow: hidden;
        }
        .bg-particles {
            position: fixed; top: 0; left: 0; width: 100%; height: 100%;
            pointer-events: none; overflow: hidden; z-index: 0;
        }
        .particle {
            position: absolute; width: 4px; height: 4px;
            background: rgba(0, 212, 255, 0.3); border-radius: 50%;
            animation: float 15s infinite;
        }
        .particle:nth-child(1) { left: 10%; animation-delay: 0s; }
        .particle:nth-child(2) { left: 20%; animation-delay: 2s; }
        .particle:nth-child(3) { left: 30%; animation-delay: 4s; }
        .particle:nth-child(4) { left: 40%; animation-delay: 6s; }
        .particle:nth-child(5) { left: 50%; animation-delay: 8s; }
        .particle:nth-child(6) { left: 60%; animation-delay: 10s; }
        .particle:nth-child(7) { left: 70%; animation-delay: 12s; }
        .particle:nth-child(8) { left: 80%; animation-delay: 14s; }
        .particle:nth-child(9) { left: 90%; animation-delay: 1s; }
        .particle:nth-child(10) { left: 95%; animation-delay: 3s; }
        @keyframes float {
            0%, 100% { transform: translateY(100vh) scale(0); opacity: 0; }
            10% { opacity: 1; } 90% { opacity: 1; }
            100% { transform: translateY(-100vh) scale(1); opacity: 0; }
        }
        .container {
            position: relative; z-index: 1; text-align: center;
            padding: 60px 50px;
            background: rgba(22, 33, 62, 0.8);
            backdrop-filter: blur(20px); border-radius: 24px;
            box-shadow: 0 25px 80px rgba(0, 0, 0, 0.5), 0 0 0 1px rgba(255, 255, 255, 0.1);
            max-width: 440px; animation: slideUp 0.6s ease-out;
        }
        @keyframes slideUp {
            from { opacity: 0; transform: translateY(30px); }
            to { opacity: 1; transform: translateY(0); }
        }
        .logo { margin-bottom: 30px; }
        .app-name {
            font-size: 28px; font-weight: 700;
            background: linear-gradient(135deg, #00d4ff, #0099ff);
            -webkit-background-clip: text; -webkit-text-fill-color: transparent;
            background-clip: text; margin-top: 12px; letter-spacing: -0.5px;
        }
        .success-icon {
            width: 90px; height: 90px; margin: 20px auto 30px;
            background: linear-gradient(135deg, #00d4ff, #00ff88);
            border-radius: 50%; display: flex;
            justify-content: center; align-items: center;
            animation: pulse 2s infinite;
            box-shadow: 0 10px 40px rgba(0, 212, 255, 0.3);
        }
        @keyframes pulse {
            0%, 100% { box-shadow: 0 10px 40px rgba(0, 212, 255, 0.3); }
            50% { box-shadow: 0 10px 60px rgba(0, 212, 255, 0.5); }
        }
        .success-icon svg {
            width: 45px; height: 45px; stroke: #fff;
            stroke-width: 3; fill: none;
            animation: checkmark 0.8s ease-out 0.3s both;
        }
        @keyframes checkmark {
            from { stroke-dashoffset: 50; }
            to { stroke-dashoffset: 0; }
        }
        .success-icon svg path { stroke-dasharray: 50; stroke-dashoffset: 0; }
        h1 { font-size: 26px; font-weight: 600; color: #fff; margin-bottom: 12px; }
        .subtitle {
            font-size: 16px; color: rgba(255, 255, 255, 0.7);
            line-height: 1.6; margin-bottom: 30px;
        }
        .provider-badge {
            display: inline-flex; align-items: center; gap: 8px;
            padding: 10px 20px; background: rgba(255, 255, 255, 0.1);
            border-radius: 30px; font-size: 14px;
            color: rgba(255, 255, 255, 0.9); margin-bottom: 30px;
        }
        .provider-badge svg { width: 20px; height: 20px; }
        .close-hint {
            font-size: 13px; color: rgba(255, 255, 255, 0.5);
            padding-top: 20px; border-top: 1px solid rgba(255, 255, 255, 0.1);
        }
        .close-hint kbd {
            display: inline-block; padding: 2px 8px;
            background: rgba(255, 255, 255, 0.1); border-radius: 4px;
            font-family: monospace; font-size: 12px; margin: 0 2px;
        }
    </style>
</head>
<body>
    <div class="bg-particles">
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
    </div>
    <div class="container">
        <div class="logo">
            <div class="app-name">AeroFTP</div>
        </div>
        <div class="success-icon">
            <svg viewBox="0 0 24 24">
                <path d="M5 13l4 4L19 7" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
        </div>
        <h1>Authorization Successful</h1>
        <p class="subtitle">Your 4shared account has been connected securely.<br>You're all set to access your files!</p>
        <div class="provider-badge">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M12 2L2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5"/>
            </svg>
            4shared Connected
        </div>
        <p class="close-hint">You can close this window and return to AeroFTP<br>or press <kbd>Alt</kbd> + <kbd>F4</kbd></p>
    </div>
</body>
</html>"#;
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;

        return Ok((oauth_token, oauth_verifier));
    }
}

/// Connect to 4shared after authentication
#[tauri::command]
pub async fn fourshared_connect(
    state: State<'_, ProviderState>,
    params: FourSharedAuthParams,
) -> Result<OAuth2ConnectResult, String> {
    use crate::providers::{FourSharedProvider, types::FourSharedConfig};

    info!("Connecting to 4shared...");

    let (access_token, access_token_secret) = load_fourshared_tokens()?;

    let config = FourSharedConfig {
        consumer_key: params.consumer_key,
        consumer_secret: params.consumer_secret,
        access_token,
        access_token_secret,
    };

    let mut provider = FourSharedProvider::new(config);
    provider.connect().await
        .map_err(|e| format!("4shared connection failed: {}", e))?;

    let display_name = provider.display_name();
    let account_email = provider.account_email();

    let mut provider_lock = state.provider.lock().await;
    *provider_lock = Some(Box::new(provider));

    info!("Connected to 4shared ({})", account_email.as_deref().unwrap_or("no email"));
    Ok(OAuth2ConnectResult { display_name, account_email })
}

/// Check if 4shared tokens exist
#[tauri::command]
pub async fn fourshared_has_tokens() -> Result<bool, String> {
    Ok(load_fourshared_tokens().is_ok())
}

/// Clear 4shared tokens (logout)
#[tauri::command]
pub async fn fourshared_logout() -> Result<(), String> {
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        let _ = store.delete(FOURSHARED_TOKEN_KEY);
    }
    info!("Logged out from 4shared");
    Ok(())
}
