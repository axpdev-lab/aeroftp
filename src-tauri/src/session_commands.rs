//! Session Commands - Tauri commands for multi-session provider management
//!
//! These commands use the new MultiProviderState to support multiple concurrent
//! provider connections. Each command accepts an optional session_id parameter.
//!
//! If session_id is not provided, the active session is used (backwards compatibility).

use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::info;

use crate::session_manager::MultiProviderState;
use crate::providers::{
    StorageProvider, ProviderFactory, ProviderType,
    ProviderConfig, RemoteEntry,
};

// ============ Request/Response Types ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConnectionParams {
    /// Unique session ID (from frontend activeSessionId)
    pub session_id: String,
    /// Protocol type: "ftp", "ftps", "webdav", "s3", "googledrive", "dropbox", "onedrive"
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
    /// OAuth client ID (for OAuth providers)
    pub client_id: Option<String>,
    /// OAuth client secret (for OAuth providers)
    pub client_secret: Option<String>,
    /// Display name override
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionListResponse {
    pub files: Vec<RemoteEntry>,
    pub current_path: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfoResponse {
    pub session_id: String,
    pub display_name: String,
    pub protocol: String,
    pub current_path: String,
    pub is_active: bool,
}

// ============ Session Lifecycle Commands ============

/// Create a new session and connect to a provider
#[tauri::command]
pub async fn session_connect(
    state: State<'_, MultiProviderState>,
    params: SessionConnectionParams,
) -> Result<SessionInfoResponse, String> {
    info!("Creating session {} for {} provider", params.session_id, params.protocol);
    
    let protocol_lower = params.protocol.to_lowercase();
    let is_oauth = matches!(
        protocol_lower.as_str(),
        "googledrive" | "google_drive" | "dropbox" | "onedrive"
    );

    // For OAuth providers, use dedicated connection flow
    if is_oauth {
        return session_connect_oauth(state, params).await;
    }

    // Build provider config for non-OAuth providers
    let provider_type = match protocol_lower.as_str() {
        "ftp" => ProviderType::Ftp,
        "ftps" => ProviderType::Ftps,
        "sftp" => ProviderType::Sftp,
        "webdav" => ProviderType::WebDav,
        "s3" => ProviderType::S3,
        "filelu" => ProviderType::FileLu,
        other => return Err(format!("Unknown protocol: {}", other)),
    };

    let mut extra = std::collections::HashMap::new();
    
    // Add S3-specific options
    if provider_type == ProviderType::S3 {
        if let Some(ref bucket) = params.bucket {
            extra.insert("bucket".to_string(), bucket.clone());
        } else {
            return Err("S3 requires a bucket name".to_string());
        }
        if let Some(ref region) = params.region {
            extra.insert("region".to_string(), region.clone());
        } else {
            extra.insert("region".to_string(), "us-east-1".to_string());
        }
        if let Some(ref endpoint) = params.endpoint {
            extra.insert("endpoint".to_string(), endpoint.clone());
        }
        if params.path_style.unwrap_or(false) {
            extra.insert("path_style".to_string(), "true".to_string());
        }
    }

    let config = ProviderConfig {
        name: params.display_name.clone()
            .unwrap_or_else(|| format!("{}@{}", params.username, params.server)),
        provider_type,
        host: params.server.clone(),
        port: params.port,
        username: Some(params.username.clone()),
        password: Some(params.password.clone()),
        initial_path: params.initial_path.clone(),
        extra,
    };

    // Create and connect provider
    let mut provider = ProviderFactory::create(&config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;
    
    provider.connect().await
        .map_err(|e| format!("Connection failed: {}", e))?;

    // Create session
    let session_info = state.create_session(
        params.session_id.clone(),
        provider,
        Some(config),
    ).await.map_err(|e| format!("Failed to create session: {}", e))?;

    let active_id = state.get_active_session_id().await;

    info!("Session {} created successfully", params.session_id);
    
    Ok(SessionInfoResponse {
        session_id: session_info.session_id,
        display_name: session_info.display_name,
        protocol: session_info.protocol,
        current_path: session_info.current_path,
        is_active: active_id.as_ref() == Some(&params.session_id),
    })
}

/// Connect OAuth provider in a session
async fn session_connect_oauth(
    state: State<'_, MultiProviderState>,
    params: SessionConnectionParams,
) -> Result<SessionInfoResponse, String> {
    use crate::providers::{GoogleDriveProvider, DropboxProvider, OneDriveProvider,
                          google_drive::GoogleDriveConfig, dropbox::DropboxConfig,
                          onedrive::OneDriveConfig};

    let client_id = params.client_id.as_ref()
        .ok_or("OAuth requires client_id")?;
    let client_secret = params.client_secret.as_ref()
        .ok_or("OAuth requires client_secret")?;

    let protocol_lower = params.protocol.to_lowercase();
    
    let provider: Box<dyn StorageProvider> = match protocol_lower.as_str() {
        "googledrive" | "google_drive" => {
            let config = GoogleDriveConfig::new(client_id, client_secret);
            let mut p = GoogleDriveProvider::new(config);
            p.connect().await
                .map_err(|e| format!("Google Drive connection failed: {}", e))?;
            Box::new(p)
        }
        "dropbox" => {
            let config = DropboxConfig::new(client_id, client_secret);
            let mut p = DropboxProvider::new(config);
            p.connect().await
                .map_err(|e| format!("Dropbox connection failed: {}", e))?;
            Box::new(p)
        }
        "onedrive" => {
            let config = OneDriveConfig::new(client_id, client_secret);
            let mut p = OneDriveProvider::new(config);
            p.connect().await
                .map_err(|e| format!("OneDrive connection failed: {}", e))?;
            Box::new(p)
        }
        _ => return Err(format!("Unknown OAuth provider: {}", params.protocol)),
    };

    // Create session
    let session_info = state.create_session(
        params.session_id.clone(),
        provider,
        None,
    ).await.map_err(|e| format!("Failed to create session: {}", e))?;

    let active_id = state.get_active_session_id().await;

    info!("OAuth session {} created successfully", params.session_id);
    
    Ok(SessionInfoResponse {
        session_id: session_info.session_id,
        display_name: session_info.display_name,
        protocol: session_info.protocol,
        current_path: session_info.current_path,
        is_active: active_id.as_ref() == Some(&params.session_id),
    })
}

/// Close a session and disconnect from the provider
#[tauri::command]
pub async fn session_disconnect(
    state: State<'_, MultiProviderState>,
    session_id: String,
) -> Result<(), String> {
    info!("Closing session {}", session_id);
    
    state.close_session(&session_id).await
        .map_err(|e| format!("Failed to close session: {}", e))?;
    
    Ok(())
}

/// Switch the active session
#[tauri::command]
pub async fn session_switch(
    state: State<'_, MultiProviderState>,
    session_id: String,
) -> Result<SessionInfoResponse, String> {
    info!("Switching to session {}", session_id);
    
    state.set_active_session(&session_id).await
        .map_err(|e| format!("Failed to switch session: {}", e))?;
    
    let info = state.get_session_info(&session_id).await
        .ok_or("Session not found after switch")?;
    
    Ok(SessionInfoResponse {
        session_id: info.session_id,
        display_name: info.display_name,
        protocol: info.protocol,
        current_path: info.current_path,
        is_active: true,
    })
}

/// List all active sessions
#[tauri::command]
pub async fn session_list(
    state: State<'_, MultiProviderState>,
) -> Result<Vec<SessionInfoResponse>, String> {
    let sessions = state.list_sessions().await;
    let active_id = state.get_active_session_id().await;
    
    Ok(sessions.into_iter().map(|info| {
        SessionInfoResponse {
            session_id: info.session_id.clone(),
            display_name: info.display_name,
            protocol: info.protocol,
            current_path: info.current_path,
            is_active: active_id.as_ref() == Some(&info.session_id),
        }
    }).collect())
}

/// Get info about a specific session
#[tauri::command]
pub async fn session_info(
    state: State<'_, MultiProviderState>,
    session_id: Option<String>,
) -> Result<SessionInfoResponse, String> {
    let sid = match session_id {
        Some(id) => id,
        None => state.get_active_session_id().await
            .ok_or("No active session")?,
    };
    
    let info = state.get_session_info(&sid).await
        .ok_or("Session not found")?;
    
    let active_id = state.get_active_session_id().await;
    
    Ok(SessionInfoResponse {
        session_id: info.session_id.clone(),
        display_name: info.display_name,
        protocol: info.protocol,
        current_path: info.current_path,
        is_active: active_id.as_ref() == Some(&info.session_id),
    })
}

// ============ File Operation Commands ============

/// List files in a session
#[tauri::command]
pub async fn session_list_files(
    state: State<'_, MultiProviderState>,
    session_id: Option<String>,
    path: Option<String>,
) -> Result<SessionListResponse, String> {
    let sid = state.resolve_session_id(session_id.as_deref()).await
        .map_err(|e| format!("Session error: {}", e))?;
    
    let list_path = path.as_deref().unwrap_or(".");
    
    let files = state.list_files_async(Some(&sid), list_path).await
        .map_err(|e| format!("Failed to list files: {}", e))?;
    
    let current_path = state.pwd(Some(&sid)).await
        .unwrap_or_else(|_| "/".to_string());
    
    Ok(SessionListResponse {
        files,
        current_path,
        session_id: sid,
    })
}

/// Change directory in a session
#[tauri::command]
pub async fn session_change_dir(
    state: State<'_, MultiProviderState>,
    session_id: Option<String>,
    path: String,
) -> Result<SessionListResponse, String> {
    let sid = state.resolve_session_id(session_id.as_deref()).await
        .map_err(|e| format!("Session error: {}", e))?;
    
    let new_path = state.change_dir(Some(&sid), &path).await
        .map_err(|e| format!("Failed to change directory: {}", e))?;
    
    let files = state.list_files_async(Some(&sid), ".").await
        .map_err(|e| format!("Failed to list files: {}", e))?;
    
    Ok(SessionListResponse {
        files,
        current_path: new_path,
        session_id: sid,
    })
}

/// Create directory in a session
#[tauri::command]
pub async fn session_mkdir(
    state: State<'_, MultiProviderState>,
    session_id: Option<String>,
    path: String,
) -> Result<(), String> {
    let sid = state.resolve_session_id(session_id.as_deref()).await
        .map_err(|e| format!("Session error: {}", e))?;
    
    state.mkdir(Some(&sid), &path).await
        .map_err(|e| format!("Failed to create directory: {}", e))?;
    
    info!("Created directory {} in session {}", path, sid);
    Ok(())
}

/// Delete file/folder in a session
#[tauri::command]
pub async fn session_delete(
    state: State<'_, MultiProviderState>,
    session_id: Option<String>,
    path: String,
) -> Result<(), String> {
    let sid = state.resolve_session_id(session_id.as_deref()).await
        .map_err(|e| format!("Session error: {}", e))?;
    
    state.delete(Some(&sid), &path).await
        .map_err(|e| format!("Failed to delete: {}", e))?;
    
    info!("Deleted {} in session {}", path, sid);
    Ok(())
}

/// Rename file/folder in a session
#[tauri::command]
pub async fn session_rename(
    state: State<'_, MultiProviderState>,
    session_id: Option<String>,
    from: String,
    to: String,
) -> Result<(), String> {
    let sid = state.resolve_session_id(session_id.as_deref()).await
        .map_err(|e| format!("Session error: {}", e))?;
    
    state.rename(Some(&sid), &from, &to).await
        .map_err(|e| format!("Failed to rename: {}", e))?;
    
    info!("Renamed {} to {} in session {}", from, to, sid);
    Ok(())
}

/// Download file in a session
#[tauri::command]
pub async fn session_download(
    state: State<'_, MultiProviderState>,
    session_id: Option<String>,
    remote_path: String,
    local_path: String,
) -> Result<(), String> {
    let sid = state.resolve_session_id(session_id.as_deref()).await
        .map_err(|e| format!("Session error: {}", e))?;
    
    state.download(Some(&sid), &remote_path, &local_path).await
        .map_err(|e| format!("Failed to download: {}", e))?;
    
    info!("Downloaded {} to {} in session {}", remote_path, local_path, sid);
    Ok(())
}

/// Upload file in a session
#[tauri::command]
pub async fn session_upload(
    state: State<'_, MultiProviderState>,
    session_id: Option<String>,
    local_path: String,
    remote_path: String,
) -> Result<(), String> {
    let sid = state.resolve_session_id(session_id.as_deref()).await
        .map_err(|e| format!("Session error: {}", e))?;
    
    state.upload(Some(&sid), &local_path, &remote_path).await
        .map_err(|e| format!("Failed to upload: {}", e))?;
    
    info!("Uploaded {} to {} in session {}", local_path, remote_path, sid);
    Ok(())
}

/// Create share link in a session
#[tauri::command]
pub async fn session_create_share_link(
    state: State<'_, MultiProviderState>,
    session_id: Option<String>,
    path: String,
) -> Result<String, String> {
    let sid = state.resolve_session_id(session_id.as_deref()).await
        .map_err(|e| format!("Session error: {}", e))?;
    
    let share_url = state.create_share_link(Some(&sid), &path).await
        .map_err(|e| format!("Failed to create share link: {}", e))?;
    
    info!("Created share link for {} in session {}: {}", path, sid, share_url);
    Ok(share_url)
}
