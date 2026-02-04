// AeroFTP - Modern FTP Client with Tauri
// Real-time transfer progress with event emission

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Instant;
use tauri::{AppHandle, Emitter, State, Manager};
use tokio::sync::Mutex;
use tracing::{info, warn, error};
use semver::Version;
use reqwest::Client as HttpClient;
use secrecy::{ExposeSecret, SecretString};

mod ftp;
mod sync;
mod ai;
mod cloud_config;
mod watcher;
mod cloud_service;
mod providers;
mod provider_commands;
mod session_manager;
mod session_commands;
mod crypto;
mod credential_store;
mod profile_export;
mod pty;
mod ssh_shell;
mod ai_tools;
mod ai_stream;
mod archive_browse;
mod aerovault;
mod aerovault_v2;
mod cryptomator;

use ftp::{FtpManager, RemoteFile};
use pty::{create_pty_state, spawn_shell, pty_write, pty_resize, pty_close};
use ssh_shell::{create_ssh_shell_state, ssh_shell_open, ssh_shell_write, ssh_shell_resize, ssh_shell_close};

/// Global transfer speed limits (bytes per second, 0 = unlimited)
pub struct SpeedLimits {
    pub download_bps: std::sync::atomic::AtomicU64,
    pub upload_bps: std::sync::atomic::AtomicU64,
}

impl SpeedLimits {
    fn new() -> Self {
        Self {
            download_bps: std::sync::atomic::AtomicU64::new(0),
            upload_bps: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

/// Apply rate limiting by sleeping after transferring a chunk.
/// Returns immediately if limit is 0 (unlimited).
pub async fn throttle_transfer(bytes_transferred: u64, elapsed: std::time::Duration, limit_bps: u64) {
    if limit_bps == 0 {
        return;
    }
    let expected_duration = std::time::Duration::from_secs_f64(bytes_transferred as f64 / limit_bps as f64);
    if expected_duration > elapsed {
        tokio::time::sleep(expected_duration - elapsed).await;
    }
}

// Shared application state
pub(crate) struct AppState {
    ftp_manager: Mutex<FtpManager>,
    cancel_flag: Mutex<bool>,
    speed_limits: SpeedLimits,
}

impl AppState {
    fn new() -> Self {
        Self {
            ftp_manager: Mutex::new(FtpManager::new()),
            cancel_flag: Mutex::new(false),
            speed_limits: SpeedLimits::new(),
        }
    }
}

// ============ Request/Response Structs ============

#[derive(Serialize, Deserialize)]
pub struct ConnectionParams {
    server: String,
    username: String,
    password: String,
}

#[derive(Serialize, Deserialize)]
pub struct DownloadParams {
    remote_path: String,
    local_path: String,
}

#[derive(Serialize, Deserialize)]
pub struct UploadParams {
    local_path: String,
    remote_path: String,
}

#[derive(Serialize, Deserialize)]
pub struct DownloadFolderParams {
    remote_path: String,
    local_path: String,
}

#[derive(Serialize, Deserialize)]
pub struct UploadFolderParams {
    local_path: String,
    remote_path: String,
}

#[derive(Serialize)]
pub struct FileListResponse {
    files: Vec<RemoteFile>,
    current_path: String,
}

// ============ Transfer Progress Events ============

#[derive(Clone, Serialize)]
pub struct TransferProgress {
    pub transfer_id: String,
    pub filename: String,
    pub transferred: u64,
    pub total: u64,
    pub percentage: u8,
    pub speed_bps: u64,
    pub eta_seconds: u32,
    pub direction: String, // "download" or "upload"
}

#[derive(Clone, Serialize)]
pub struct TransferEvent {
    pub event_type: String, // "start", "progress", "complete", "error", "cancelled"
    pub transfer_id: String,
    pub filename: String,
    pub direction: String,
    pub message: Option<String>,
    pub progress: Option<TransferProgress>,
}

// ============ Local File Info ============

#[derive(Serialize)]
pub struct LocalFileInfo {
    pub name: String,
    pub path: String,
    pub size: Option<u64>,
    pub is_dir: bool,
    pub modified: Option<String>,
}

// ============ Updater Structs ============

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Serialize)]
struct UpdateInfo {
    has_update: bool,
    latest_version: Option<String>,
    download_url: Option<String>,
    current_version: String,
    install_format: String,
}

// ============ Updater Command ============

/// Detect how the app was installed (deb, appimage, snap, flatpak, rpm, exe, dmg)
fn detect_install_format() -> String {
    let os = std::env::consts::OS;
    
    match os {
        "linux" => {
            // Check for Snap
            if std::env::var("SNAP").is_ok() {
                return "snap".to_string();
            }
            // Check for Flatpak
            if std::env::var("FLATPAK_ID").is_ok() {
                return "flatpak".to_string();
            }
            // Check for AppImage - the executable path contains "AppImage"
            if let Ok(exe_path) = std::env::current_exe() {
                let path_str = exe_path.to_string_lossy();
                if path_str.contains("AppImage") || path_str.contains(".AppImage") {
                    return "appimage".to_string();
                }
            }
            // Check for RPM-based distros (Fedora, CentOS, RHEL)
            if std::path::Path::new("/etc/redhat-release").exists() 
                || std::path::Path::new("/etc/fedora-release").exists() {
                return "rpm".to_string();
            }
            // Default to DEB for Debian/Ubuntu based
            "deb".to_string()
        }
        "windows" => {
            // Check if installed via MSI (usually in Program Files)
            if let Ok(exe_path) = std::env::current_exe() {
                let path_str = exe_path.to_string_lossy().to_lowercase();
                if path_str.contains("program files") {
                    return "msi".to_string();
                }
            }
            "exe".to_string()
        }
        "macos" => "dmg".to_string(),
        _ => "unknown".to_string(),
    }
}

#[tauri::command]
fn copy_to_clipboard(text: String) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| format!("Clipboard init failed: {}", e))?;
    #[cfg(target_os = "linux")]
    {
        use arboard::SetExtLinux;
        clipboard.set().wait().text(text)
            .map_err(|e| format!("Clipboard write failed: {}", e))?;
    }
    #[cfg(not(target_os = "linux"))]
    {
        clipboard.set_text(text)
            .map_err(|e| format!("Clipboard write failed: {}", e))?;
    }
    Ok(())
}

#[tauri::command]
async fn check_update() -> Result<UpdateInfo, String> {
    let current_version = env!("CARGO_PKG_VERSION");
    let install_format = detect_install_format();
    
    info!("Checking for updates... Current: v{}, Format: {}", current_version, install_format);
    
    let client = HttpClient::new();
    let url = "https://api.github.com/repos/axpnet/aeroftp/releases/latest";
    
    let response = client.get(url)
        .header("User-Agent", "AeroFTP")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch releases: {}", e))?;
    
    if !response.status().is_success() {
        return Err(format!("GitHub API error: {}", response.status()));
    }
    
    let release: GitHubRelease = response.json()
        .await
        .map_err(|e| format!("Failed to parse release info: {}", e))?;
    
    // Parse versions (remove 'v' prefix if present)
    let latest_tag = release.tag_name.trim_start_matches('v');
    let current = Version::parse(current_version)
        .map_err(|e| format!("Failed to parse current version: {}", e))?;
    let latest = Version::parse(latest_tag)
        .map_err(|e| format!("Failed to parse latest version: {}", e))?;
    
    if latest > current {
        // Find asset matching the installed format
        let extension = match install_format.as_str() {
            "deb" => ".deb",
            "rpm" => ".rpm",
            "appimage" => ".appimage",
            "snap" => ".snap",
            "flatpak" => ".flatpak",
            "exe" => ".exe",
            "msi" => ".msi",
            "dmg" => ".dmg",
            _ => "",
        };
        
        let download_url = if !extension.is_empty() {
            release.assets.iter()
                .find(|a| a.name.to_lowercase().ends_with(extension))
                .map(|a| a.browser_download_url.clone())
        } else {
            None
        };
        
        info!("Update available: v{} -> v{} (format: {}, url: {:?})", 
              current_version, latest_tag, install_format, download_url);
        
        return Ok(UpdateInfo {
            has_update: true,
            latest_version: Some(latest_tag.to_string()),
            download_url,
            current_version: current_version.to_string(),
            install_format,
        });
    }
    
    info!("No update available. Current: v{}, Latest: v{}", current_version, latest_tag);
    
    Ok(UpdateInfo {
        has_update: false,
        latest_version: Some(latest_tag.to_string()),
        download_url: None,
        current_version: current_version.to_string(),
        install_format,
    })
}

#[tauri::command]
fn log_update_detection(version: String) {
    info!("New version detected: v{}", version);
}

/// Download an update file with progress events
#[tauri::command]
async fn download_update(app: AppHandle, url: String) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;

    info!("Downloading update from: {}", url);

    let client = HttpClient::new();
    let response = client.get(&url)
        .header("User-Agent", "AeroFTP")
        .send()
        .await
        .map_err(|e| format!("Download request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("Download failed: HTTP {}", response.status()));
    }

    let total_size = response.content_length().unwrap_or(0);
    let filename = url.rsplit('/').next().unwrap_or("aeroftp-update");

    // Save to Downloads directory or temp
    let download_dir = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .unwrap_or_else(std::env::temp_dir);
    let dest_path = download_dir.join(filename);

    let mut file = tokio::fs::File::create(&dest_path)
        .await
        .map_err(|e| format!("Cannot create file: {}", e))?;

    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;
    let start = std::time::Instant::now();
    let mut last_emit = std::time::Instant::now();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download stream error: {}", e))?;
        file.write_all(&chunk).await.map_err(|e| format!("Write error: {}", e))?;
        downloaded += chunk.len() as u64;

        // Emit progress every 100ms to avoid flooding
        if last_emit.elapsed().as_millis() >= 100 {
            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 { downloaded as f64 / elapsed } else { 0.0 };
            let percentage = if total_size > 0 { (downloaded as f64 / total_size as f64 * 100.0) as u8 } else { 0 };
            let eta = if speed > 0.0 && total_size > 0 { ((total_size - downloaded) as f64 / speed) as u64 } else { 0 };

            let _ = app.emit("update-download-progress", serde_json::json!({
                "downloaded": downloaded,
                "total": total_size,
                "percentage": percentage,
                "speed_bps": speed as u64,
                "eta_seconds": eta,
                "filename": filename,
            }));
            last_emit = std::time::Instant::now();
        }
    }

    file.flush().await.map_err(|e| format!("Flush error: {}", e))?;

    // Final 100% emit
    let _ = app.emit("update-download-progress", serde_json::json!({
        "downloaded": total_size,
        "total": total_size,
        "percentage": 100,
        "speed_bps": 0,
        "eta_seconds": 0,
        "filename": filename,
    }));

    let path_str = dest_path.to_string_lossy().to_string();
    info!("Update downloaded to: {}", path_str);
    Ok(path_str)
}

/// Replace current AppImage with downloaded update and restart
#[tauri::command]
async fn install_appimage_update(app: AppHandle, downloaded_path: String) -> Result<(), String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Cannot find current exe: {}", e))?;

    let current_str = current_exe.to_string_lossy().to_lowercase();
    if !current_str.contains("appimage") {
        return Err("Not running as AppImage — manual install required".to_string());
    }

    let downloaded = std::path::Path::new(&downloaded_path);
    if !downloaded.exists() {
        return Err("Downloaded file not found".to_string());
    }

    info!("AppImage auto-update: {} -> {}", downloaded_path, current_exe.display());

    // Backup current AppImage
    let backup = current_exe.with_extension("bak");
    std::fs::rename(&current_exe, &backup)
        .map_err(|e| format!("Backup failed: {}", e))?;

    // Move downloaded file to current exe path
    if let Err(e) = std::fs::copy(downloaded, &current_exe) {
        // Restore backup on failure
        let _ = std::fs::rename(&backup, &current_exe);
        return Err(format!("Replace failed: {}", e));
    }

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&current_exe, std::fs::Permissions::from_mode(0o755));
    }

    // Remove backup and downloaded file
    let _ = std::fs::remove_file(&backup);
    let _ = std::fs::remove_file(downloaded);

    info!("AppImage updated successfully, restarting...");

    // Restart: spawn new process then exit
    let _ = std::process::Command::new(&current_exe).spawn();
    app.exit(0);

    Ok(())
}

// ============ FTP Commands ============

#[tauri::command]
async fn connect_ftp(state: State<'_, AppState>, params: ConnectionParams) -> Result<(), String> {
    info!("Connecting to FTP server: {}", params.server);
    let mut ftp_manager = state.ftp_manager.lock().await;
    
    ftp_manager.connect(&params.server)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;
        
    ftp_manager.login(&params.username, &params.password)
        .await
        .map_err(|e| format!("Login failed: {}", e))?;
        
    Ok(())
}

#[tauri::command]
async fn disconnect_ftp(state: State<'_, AppState>) -> Result<(), String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    ftp_manager.disconnect()
        .await
        .map_err(|e| format!("Disconnect failed: {}", e))?;
    Ok(())
}

#[tauri::command]
async fn check_connection(state: State<'_, AppState>) -> Result<bool, String> {
    let ftp_manager = state.ftp_manager.lock().await;
    Ok(ftp_manager.is_connected())
}

#[tauri::command]
async fn ftp_noop(state: State<'_, AppState>) -> Result<(), String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    ftp_manager.noop()
        .await
        .map_err(|e| format!("NOOP failed: {}", e))?;
    Ok(())
}

#[tauri::command]
async fn reconnect_ftp(state: State<'_, AppState>) -> Result<(), String> {
    info!("Attempting FTP reconnection");
    let mut ftp_manager = state.ftp_manager.lock().await;
    ftp_manager.reconnect()
        .await
        .map_err(|e| format!("Reconnection failed: {}", e))?;
    info!("FTP reconnection successful");
    Ok(())
}

#[tauri::command]
async fn list_files(state: State<'_, AppState>) -> Result<FileListResponse, String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    
    let files = ftp_manager.list_files()
        .await
        .map_err(|e| format!("Failed to list files: {}", e))?;
        
    let current_path = ftp_manager.current_path();
    
    Ok(FileListResponse {
        files,
        current_path,
    })
}

#[tauri::command]
async fn change_directory(state: State<'_, AppState>, path: String) -> Result<FileListResponse, String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    
    ftp_manager.change_dir(&path)
        .await
        .map_err(|e| format!("Failed to change directory: {}", e))?;
        
    let files = ftp_manager.list_files()
        .await
        .map_err(|e| format!("Failed to list files: {}", e))?;
        
    let current_path = ftp_manager.current_path();
    
    Ok(FileListResponse {
        files,
        current_path,
    })
}

// ============ Transfer Commands with Progress ============

#[tauri::command]
async fn download_file(
    app: AppHandle,
    state: State<'_, AppState>, 
    params: DownloadParams
) -> Result<String, String> {
    // Reset cancel flag
    {
        let mut cancel = state.cancel_flag.lock().await;
        *cancel = false;
    }

    let filename = PathBuf::from(&params.remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    
    let transfer_id = format!("dl-{}", chrono::Utc::now().timestamp_millis());
    
    // Emit start event
    let _ = app.emit("transfer_event", TransferEvent {
        event_type: "start".to_string(),
        transfer_id: transfer_id.clone(),
        filename: filename.clone(),
        direction: "download".to_string(),
        message: Some(format!("Starting download: {}", filename)),
        progress: None,
    });

    let mut ftp_manager = state.ftp_manager.lock().await;
    
    // Get file size first
    let file_size = ftp_manager.get_file_size(&params.remote_path)
        .await
        .unwrap_or(0);
    
    let start_time = Instant::now();

    // Download with progress
    match ftp_manager.download_file_with_progress(
        &params.remote_path, 
        &params.local_path,
        |transferred| {
            let elapsed = start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 { (transferred as f64 / elapsed) as u64 } else { 0 };
            let percentage = if file_size > 0 { 
                ((transferred as f64 / file_size as f64) * 100.0) as u8 
            } else { 
                0 
            };
            let eta = if speed > 0 && file_size > transferred {
                ((file_size - transferred) / speed) as u32
            } else {
                0
            };

            let progress = TransferProgress {
                transfer_id: transfer_id.clone(),
                filename: filename.clone(),
                transferred,
                total: file_size,
                percentage,
                speed_bps: speed,
                eta_seconds: eta,
                direction: "download".to_string(),
            };

            let _ = app.emit("transfer_event", TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: transfer_id.clone(),
                filename: filename.clone(),
                direction: "download".to_string(),
                message: None,
                progress: Some(progress),
            });
        }
    ).await {
        Ok(_) => {
            // Emit complete event
            let _ = app.emit("transfer_event", TransferEvent {
                event_type: "complete".to_string(),
                transfer_id: transfer_id.clone(),
                filename: filename.clone(),
                direction: "download".to_string(),
                message: Some(format!("Download complete: {}", filename)),
                progress: None,
            });
            Ok(format!("Downloaded: {}", filename))
        }
        Err(e) => {
            // Emit error event
            let _ = app.emit("transfer_event", TransferEvent {
                event_type: "error".to_string(),
                transfer_id: transfer_id.clone(),
                filename: filename.clone(),
                direction: "download".to_string(),
                message: Some(format!("Download failed: {}", e)),
                progress: None,
            });
            Err(format!("Download failed: {}", e))
        }
    }
}

#[tauri::command]
async fn upload_file(
    app: AppHandle,
    state: State<'_, AppState>, 
    params: UploadParams
) -> Result<String, String> {
    // Reset cancel flag
    {
        let mut cancel = state.cancel_flag.lock().await;
        *cancel = false;
    }

    let filename = PathBuf::from(&params.local_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    
    let transfer_id = format!("ul-{}", chrono::Utc::now().timestamp_millis());
    
    // Get local file size
    let file_size = tokio::fs::metadata(&params.local_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    // Emit start event
    let _ = app.emit("transfer_event", TransferEvent {
        event_type: "start".to_string(),
        transfer_id: transfer_id.clone(),
        filename: filename.clone(),
        direction: "upload".to_string(),
        message: Some(format!("Starting upload: {}", filename)),
        progress: None,
    });

    let mut ftp_manager = state.ftp_manager.lock().await;
    let start_time = Instant::now();

    // Upload with progress
    match ftp_manager.upload_file_with_progress(
        &params.local_path, 
        &params.remote_path,
        file_size,
        |transferred| {
            let elapsed = start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 { (transferred as f64 / elapsed) as u64 } else { 0 };
            let percentage = if file_size > 0 { 
                ((transferred as f64 / file_size as f64) * 100.0) as u8 
            } else { 
                0 
            };
            let eta = if speed > 0 && file_size > transferred {
                ((file_size - transferred) / speed) as u32
            } else {
                0
            };

            let progress = TransferProgress {
                transfer_id: transfer_id.clone(),
                filename: filename.clone(),
                transferred,
                total: file_size,
                percentage,
                speed_bps: speed,
                eta_seconds: eta,
                direction: "upload".to_string(),
            };

            let _ = app.emit("transfer_event", TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: transfer_id.clone(),
                filename: filename.clone(),
                direction: "upload".to_string(),
                message: None,
                progress: Some(progress),
            });
        }
    ).await {
        Ok(_) => {
            // Emit complete event
            let _ = app.emit("transfer_event", TransferEvent {
                event_type: "complete".to_string(),
                transfer_id: transfer_id.clone(),
                filename: filename.clone(),
                direction: "upload".to_string(),
                message: Some(format!("Upload complete: {}", filename)),
                progress: None,
            });
            Ok(format!("Uploaded: {}", filename))
        }
        Err(e) => {
            // Emit error event
            let _ = app.emit("transfer_event", TransferEvent {
                event_type: "error".to_string(),
                transfer_id: transfer_id.clone(),
                filename: filename.clone(),
                direction: "upload".to_string(),
                message: Some(format!("Upload failed: {}", e)),
                progress: None,
            });
            Err(format!("Upload failed: {}", e))
        }
    }
}

#[tauri::command]
async fn download_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    params: DownloadFolderParams
) -> Result<String, String> {
    
    info!("Downloading folder: {} -> {}", params.remote_path, params.local_path);
    
    let folder_name = PathBuf::from(&params.remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".to_string());
    
    let transfer_id = format!("dl-folder-{}", chrono::Utc::now().timestamp_millis());
    
    // Emit start event
    let _ = app.emit("transfer_event", TransferEvent {
        event_type: "start".to_string(),
        transfer_id: transfer_id.clone(),
        filename: folder_name.clone(),
        direction: "download".to_string(),
        message: Some(format!("Starting folder download: {}", folder_name)),
        progress: None,
    });
    
    // Create local directory
    let local_folder_path = PathBuf::from(&params.local_path);
    
    if let Err(e) = tokio::fs::create_dir_all(&local_folder_path).await {
        let _ = app.emit("transfer_event", TransferEvent {
            event_type: "error".to_string(),
            transfer_id: transfer_id.clone(),
            filename: folder_name.clone(),
            direction: "download".to_string(),
            message: Some(format!("Failed to create local directory: {}", e)),
            progress: None,
        });
        return Err(format!("Failed to create local directory: {}", e));
    }
    
    // Get file list from remote folder
    let mut ftp_manager = state.ftp_manager.lock().await;
    let original_path = ftp_manager.current_path();
    
    // First, collect all files recursively using a stack-based approach
    // This avoids deep recursion and collects full inventory first
    
    #[derive(Debug, Clone)]
    struct DownloadItem {
        remote_path: String,      // Full remote path
        local_path: PathBuf,      // Full local path
        is_dir: bool,
        size: u64,
        name: String,
    }
    
    let mut items_to_download: Vec<DownloadItem> = Vec::new();
    let mut dirs_to_scan: Vec<(String, PathBuf)> = vec![(params.remote_path.clone(), local_folder_path.clone())];
    let mut scan_counter: u64 = 0;
    let mut last_scan_emit = std::time::Instant::now();
    
    // Scan phase: collect all files and directories recursively
    while let Some((remote_dir, local_dir)) = dirs_to_scan.pop() {
        // Change to remote directory
        if let Err(e) = ftp_manager.change_dir(&remote_dir).await {
            warn!("Cannot access remote directory {}: {}", remote_dir, e);
            continue;
        }
        
        // List files
        let files = match ftp_manager.list_files().await {
            Ok(f) => f,
            Err(e) => {
                warn!("Cannot list files in {}: {}", remote_dir, e);
                continue;
            }
        };
        
        for file in files {
            let remote_file_path = format!("{}/{}", remote_dir.trim_end_matches('/'), file.name);
            let local_file_path = local_dir.join(&file.name);
            
            if file.is_dir {
                // Add directory to scan queue
                dirs_to_scan.push((remote_file_path.clone(), local_file_path.clone()));
                // Also add it as an item so we create the local directory
                items_to_download.push(DownloadItem {
                    remote_path: remote_file_path,
                    local_path: local_file_path,
                    is_dir: true,
                    size: 0,
                    name: file.name,
                });
            } else {
                // Add file to download list
                items_to_download.push(DownloadItem {
                    remote_path: remote_file_path,
                    local_path: local_file_path,
                    is_dir: false,
                    size: file.size.unwrap_or(0),
                    name: file.name,
                });
            }
            
            scan_counter += 1;
            
            // Emit scan progress every 500ms or every 100 files
            if last_scan_emit.elapsed().as_millis() > 500 || scan_counter % 100 == 0 {
                let files_found = items_to_download.iter().filter(|i| !i.is_dir).count();
                let dirs_found = items_to_download.iter().filter(|i| i.is_dir).count();
                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "download".to_string(),
                    message: Some(format!("Scanning... {} files, {} folders found", files_found, dirs_found)),
                    progress: None,
                });
                last_scan_emit = std::time::Instant::now();
            }
        }
    }
    
    // Sort items: directories first (to create them), then files
    items_to_download.sort_by(|a, b| {
        match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.remote_path.cmp(&b.remote_path),
        }
    });
    
    let total_files = items_to_download.iter().filter(|i| !i.is_dir).count();
    let total_dirs = items_to_download.iter().filter(|i| i.is_dir).count();
    let total_size: u64 = items_to_download.iter().map(|i| i.size).sum();
    
    info!("Found {} files and {} directories to download (total size: {} bytes)", 
          total_files, total_dirs, total_size);
    
    // Emit scan complete event
    let _ = app.emit("transfer_event", TransferEvent {
        event_type: "progress".to_string(),
        transfer_id: transfer_id.clone(),
        filename: folder_name.clone(),
        direction: "download".to_string(),
        message: Some(format!("Found {} files in {} folders", total_files, total_dirs)),
        progress: None,
    });
    
    // Download phase: process all items
    let mut downloaded_files = 0;
    let mut errors = 0;
    
    for item in &items_to_download {
        if item.is_dir {
            // Create local directory
            if let Err(e) = tokio::fs::create_dir_all(&item.local_path).await {
                warn!("Failed to create directory {}: {}", item.local_path.display(), e);
                errors += 1;
            }
        } else {
            // Download file
            // First, change to the file's parent directory on the server
            if let Some(parent) = PathBuf::from(&item.remote_path).parent() {
                let parent_str = parent.to_string_lossy().to_string();
                if !parent_str.is_empty() {
                    let _ = ftp_manager.change_dir(&parent_str).await;
                }
            }
            
            // Emit file start event
            let file_transfer_id = format!("{}-{}", transfer_id, downloaded_files);
            let _ = app.emit("transfer_event", TransferEvent {
                event_type: "file_start".to_string(),
                transfer_id: file_transfer_id.clone(),
                filename: item.name.clone(),
                direction: "download".to_string(),
                message: Some(format!("Downloading: {}", item.name)),
                progress: Some(TransferProgress {
                    transfer_id: file_transfer_id.clone(),
                    filename: item.name.clone(),
                    transferred: 0,
                    total: item.size,
                    percentage: 0,
                    speed_bps: 0,
                    eta_seconds: 0,
                    direction: "download".to_string(),
                }),
            });
            
            // Download the file
            let file_name_only = PathBuf::from(&item.remote_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| item.name.clone());
                
            match ftp_manager.download_file(&file_name_only, item.local_path.to_string_lossy().as_ref()).await {
                Ok(_) => {
                    downloaded_files += 1;
                    
                    let percentage = if total_files > 0 {
                        ((downloaded_files as f64 / total_files as f64) * 100.0) as u8
                    } else {
                        100
                    };
                    
                    // Emit file complete event
                    let _ = app.emit("transfer_event", TransferEvent {
                        event_type: "file_complete".to_string(),
                        transfer_id: file_transfer_id.clone(),
                        filename: item.name.clone(),
                        direction: "download".to_string(),
                        message: Some(format!("Downloaded: {} ({}/{})", item.name, downloaded_files, total_files)),
                        progress: Some(TransferProgress {
                            transfer_id: transfer_id.clone(),
                            filename: item.name.clone(),
                            transferred: item.size,
                            total: item.size,
                            percentage,
                            speed_bps: 0,
                            eta_seconds: 0,
                            direction: "download".to_string(),
                        }),
                    });
                    
                    info!("Downloaded: {} ({}/{})", item.name, downloaded_files, total_files);
                }
                Err(e) => {
                    errors += 1;
                    warn!("Failed to download {}: {}", item.name, e);
                    
                    // Emit file error event
                    let _ = app.emit("transfer_event", TransferEvent {
                        event_type: "file_error".to_string(),
                        transfer_id: file_transfer_id,
                        filename: item.name.clone(),
                        direction: "download".to_string(),
                        message: Some(format!("Failed: {} - {}", item.name, e)),
                        progress: None,
                    });
                }
            }
        }
    }
    
    // Return to original directory
    let _ = ftp_manager.change_dir(&original_path).await;
    
    // Emit complete event
    let result_message = if errors > 0 {
        format!("Downloaded {} files ({} errors)", downloaded_files, errors)
    } else {
        format!("Downloaded {} files successfully", downloaded_files)
    };
    
    let _ = app.emit("transfer_event", TransferEvent {
        event_type: "complete".to_string(),
        transfer_id: transfer_id.clone(),
        filename: folder_name.clone(),
        direction: "download".to_string(),
        message: Some(result_message.clone()),
        progress: None,
    });
    
    Ok(result_message)
}

/// Upload an entire folder to the FTP server with full recursive support.
/// Uses stack-based iterative traversal to upload ALL files in ALL subdirectories.
/// Emits per-file events for activity log visibility.
#[tauri::command]
async fn upload_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    params: UploadFolderParams
) -> Result<String, String> {
    
    info!("Uploading folder recursively: {} -> {}", params.local_path, params.remote_path);
    
    let folder_name = PathBuf::from(&params.local_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".to_string());
    
    let transfer_id = format!("ul-folder-{}", chrono::Utc::now().timestamp_millis());
    
    // Emit folder upload start event
    let _ = app.emit("transfer_event", TransferEvent {
        event_type: "start".to_string(),
        transfer_id: transfer_id.clone(),
        filename: folder_name.clone(),
        direction: "upload".to_string(),
        message: Some(format!("Scanning folder: {}", folder_name)),
        progress: None,
    });
    
    let local_base_path = PathBuf::from(&params.local_path);
    
    if !local_base_path.is_dir() {
        let _ = app.emit("transfer_event", TransferEvent {
            event_type: "error".to_string(),
            transfer_id: transfer_id.clone(),
            filename: folder_name.clone(),
            direction: "upload".to_string(),
            message: Some("Source is not a directory".to_string()),
            progress: None,
        });
        return Err("Source is not a directory".to_string());
    }
    
    // Get FTP connection
    let mut ftp_manager = state.ftp_manager.lock().await;
    let current_remote_path = ftp_manager.current_path();
    
    // Determine remote base folder path
    let remote_base_path = if params.remote_path.is_empty() || params.remote_path == "." {
        if current_remote_path == "/" {
            format!("/{}", folder_name)
        } else {
            format!("{}/{}", current_remote_path, folder_name)
        }
    } else {
        params.remote_path.clone()
    };
    
    // ============ PHASE 1: Recursively scan ALL local files and directories ============
    // Using stack-based traversal instead of recursion for better control
    
    struct UploadItem {
        local_path: PathBuf,
        remote_path: String,
        is_dir: bool,
        size: u64,
        name: String,
    }
    
    let mut items_to_upload: Vec<UploadItem> = Vec::new();
    let mut dirs_to_create: Vec<String> = Vec::new();
    
    // Stack for directory traversal: (local_dir_path, remote_dir_path)
    let mut dirs_to_scan: Vec<(PathBuf, String)> = vec![(local_base_path.clone(), remote_base_path.clone())];
    
    // Add the root folder to create
    dirs_to_create.push(remote_base_path.clone());
    
    info!("Phase 1: Scanning local directory structure...");
    
    let mut scan_counter: u64 = 0;
    let mut last_scan_emit = std::time::Instant::now();
    
    while let Some((current_local_dir, current_remote_dir)) = dirs_to_scan.pop() {
        let mut read_dir = match tokio::fs::read_dir(&current_local_dir).await {
            Ok(rd) => rd,
            Err(e) => {
                warn!("Failed to read directory {:?}: {}", current_local_dir, e);
                continue;
            }
        };
        
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let local_path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let remote_path = format!("{}/{}", current_remote_dir, name);
            
            if local_path.is_dir() {
                // Queue this directory for scanning
                dirs_to_scan.push((local_path.clone(), remote_path.clone()));
                // Add directory to create list
                dirs_to_create.push(remote_path.clone());
                
                items_to_upload.push(UploadItem {
                    local_path,
                    remote_path,
                    is_dir: true,
                    size: 0,
                    name,
                });
            } else if local_path.is_file() {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                
                items_to_upload.push(UploadItem {
                    local_path,
                    remote_path,
                    is_dir: false,
                    size,
                    name,
                });
            }
            
            scan_counter += 1;
            
            // Emit scan progress every 500ms or every 100 files
            if last_scan_emit.elapsed().as_millis() > 500 || scan_counter % 100 == 0 {
                let files_found = items_to_upload.iter().filter(|i| !i.is_dir).count();
                let dirs_found = dirs_to_create.len();
                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!("Scanning... {} files, {} folders found", files_found, dirs_found)),
                    progress: None,
                });
                last_scan_emit = std::time::Instant::now();
            }
        }
    }
    
    // Separate files from directories
    let files_to_upload: Vec<&UploadItem> = items_to_upload.iter()
        .filter(|item| !item.is_dir)
        .collect();
    
    let total_files = files_to_upload.len();
    let total_dirs = dirs_to_create.len();
    let total_size: u64 = files_to_upload.iter().map(|f| f.size).sum();
    
    info!("Phase 1 complete: Found {} files in {} directories (total: {} bytes)", 
          total_files, total_dirs, total_size);
    
    // Update event with scan results
    let _ = app.emit("transfer_event", TransferEvent {
        event_type: "progress".to_string(),
        transfer_id: transfer_id.clone(),
        filename: folder_name.clone(),
        direction: "upload".to_string(),
        message: Some(format!("Found {} files in {} folders to upload", total_files, total_dirs)),
        progress: None,
    });
    
    // ============ PHASE 2: Create all remote directories first ============
    info!("Phase 2: Creating {} remote directories...", total_dirs);
    
    // Sort directories by depth (shortest first) to ensure parent dirs exist
    let mut dirs_sorted = dirs_to_create.clone();
    dirs_sorted.sort_by(|a, b| a.matches('/').count().cmp(&b.matches('/').count()));
    
    for remote_dir in &dirs_sorted {
        match ftp_manager.mkdir(remote_dir).await {
            Ok(_) => info!("Created remote directory: {}", remote_dir),
            Err(e) => {
                // Ignore "directory exists" errors
                let err_str = e.to_string().to_lowercase();
                if !err_str.contains("exist") && !err_str.contains("550") {
                    warn!("Could not create directory {}: {}", remote_dir, e);
                }
            }
        }
    }
    
    // ============ PHASE 3: Upload all files with per-file events ============
    info!("Phase 3: Uploading {} files...", total_files);
    
    let mut uploaded_files = 0u64;
    let mut errors = 0u64;
    
    for item in &files_to_upload {
        let file_transfer_id = format!("ul-{}-{}", transfer_id, uploaded_files);
        
        // Emit file_start event for activity log
        let _ = app.emit("transfer_event", TransferEvent {
            event_type: "file_start".to_string(),
            transfer_id: file_transfer_id.clone(),
            filename: item.name.clone(),
            direction: "upload".to_string(),
            message: Some(format!("Uploading: {}", item.remote_path)),
            progress: Some(TransferProgress {
                transfer_id: file_transfer_id.clone(),
                filename: item.name.clone(),
                transferred: 0,
                total: item.size,
                percentage: 0,
                speed_bps: 0,
                eta_seconds: 0,
                direction: "upload".to_string(),
            }),
        });
        
        info!("Uploading [{}/{}]: {} -> {}", 
              uploaded_files + 1, total_files, item.local_path.display(), item.remote_path);
        
        match ftp_manager.upload_file(item.local_path.to_string_lossy().as_ref(), &item.remote_path).await {
            Ok(_) => {
                uploaded_files += 1;
                let percentage = if total_files > 0 {
                    ((uploaded_files as f64 / total_files as f64) * 100.0) as u8
                } else {
                    100
                };
                
                // Emit file_complete event
                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "file_complete".to_string(),
                    transfer_id: file_transfer_id.clone(),
                    filename: item.name.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!("Uploaded: {} ({} bytes)", item.name, item.size)),
                    progress: Some(TransferProgress {
                        transfer_id: file_transfer_id,
                        filename: item.name.clone(),
                        transferred: item.size,
                        total: item.size,
                        percentage: 100,
                        speed_bps: 0,
                        eta_seconds: 0,
                        direction: "upload".to_string(),
                    }),
                });
                
                // Emit folder progress event
                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!("Uploaded {}/{} files", uploaded_files, total_files)),
                    progress: Some(TransferProgress {
                        transfer_id: transfer_id.clone(),
                        filename: folder_name.clone(),
                        transferred: uploaded_files,
                        total: total_files as u64,
                        percentage,
                        speed_bps: 0,
                        eta_seconds: 0,
                        direction: "upload".to_string(),
                    }),
                });
            }
            Err(e) => {
                errors += 1;
                warn!("Failed to upload file {}: {}", item.name, e);
                
                // Emit file_error event
                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "file_error".to_string(),
                    transfer_id: file_transfer_id,
                    filename: item.name.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!("Failed to upload {}: {}", item.name, e)),
                    progress: None,
                });
            }
        }
    }
    
    // Emit complete event
    let result_message = if errors > 0 {
        format!("Uploaded {} files ({} errors)", uploaded_files, errors)
    } else {
        format!("Uploaded {} files successfully", uploaded_files)
    };
    
    let _ = app.emit("transfer_event", TransferEvent {
        event_type: "complete".to_string(),
        transfer_id: transfer_id.clone(),
        filename: folder_name.clone(),
        direction: "upload".to_string(),
        message: Some(result_message.clone()),
        progress: None,
    });
    
    Ok(result_message)
}

#[tauri::command]
async fn cancel_transfer(state: State<'_, AppState>) -> Result<(), String> {
    let mut cancel = state.cancel_flag.lock().await;
    *cancel = true;
    info!("Transfer cancellation requested");
    Ok(())
}

// ============ Bandwidth Throttling ============

/// Set global transfer speed limits (KB/s, 0 = unlimited)
#[tauri::command]
async fn set_speed_limit(
    state: State<'_, AppState>,
    download_kb: u64,
    upload_kb: u64,
) -> Result<(), String> {
    state.speed_limits.download_bps.store(
        download_kb * 1024,
        std::sync::atomic::Ordering::Relaxed,
    );
    state.speed_limits.upload_bps.store(
        upload_kb * 1024,
        std::sync::atomic::Ordering::Relaxed,
    );
    info!("Speed limits set: download={}KB/s upload={}KB/s (0=unlimited)", download_kb, upload_kb);
    Ok(())
}

/// Get current global transfer speed limits (KB/s)
#[tauri::command]
async fn get_speed_limit(
    state: State<'_, AppState>,
) -> Result<(u64, u64), String> {
    let dl = state.speed_limits.download_bps.load(std::sync::atomic::Ordering::Relaxed) / 1024;
    let ul = state.speed_limits.upload_bps.load(std::sync::atomic::Ordering::Relaxed) / 1024;
    Ok((dl, ul))
}

// ============ Environment Detection ============

/// Check if the application is running as a Snap package
#[tauri::command]
fn is_running_as_snap() -> bool {
    std::env::var("SNAP").is_ok()
}

// ============ Debug & Dependencies Commands ============

#[derive(Clone, serde::Serialize)]
struct DependencyInfo {
    name: String,
    version: String,
    category: String,
}

#[derive(Clone, serde::Serialize)]
struct CrateVersionResult {
    name: String,
    latest_version: Option<String>,
    error: Option<String>,
}

#[derive(Clone, serde::Serialize)]
struct SystemInfo {
    app_version: String,
    os: String,
    os_version: String,
    arch: String,
    tauri_version: String,
    rust_version: String,
    keyring_backend: String,
    config_dir: String,
    vault_exists: bool,
    known_hosts_exists: bool,
    dep_versions: std::collections::HashMap<String, String>,
}

#[tauri::command]
fn get_dependencies() -> Vec<DependencyInfo> {
    vec![
        // Core Framework (versions from Cargo.lock via build.rs)
        DependencyInfo { name: "tauri".into(), version: env!("DEP_VERSION_TAURI").into(), category: "Core".into() },
        DependencyInfo { name: "tokio".into(), version: env!("DEP_VERSION_TOKIO").into(), category: "Core".into() },
        DependencyInfo { name: "serde".into(), version: env!("DEP_VERSION_SERDE").into(), category: "Core".into() },
        DependencyInfo { name: "serde_json".into(), version: env!("DEP_VERSION_SERDE_JSON").into(), category: "Core".into() },
        DependencyInfo { name: "anyhow".into(), version: env!("DEP_VERSION_ANYHOW").into(), category: "Core".into() },
        DependencyInfo { name: "thiserror".into(), version: env!("DEP_VERSION_THISERROR").into(), category: "Core".into() },
        DependencyInfo { name: "chrono".into(), version: env!("DEP_VERSION_CHRONO").into(), category: "Core".into() },
        DependencyInfo { name: "log".into(), version: env!("DEP_VERSION_LOG").into(), category: "Core".into() },
        DependencyInfo { name: "tracing".into(), version: env!("DEP_VERSION_TRACING").into(), category: "Core".into() },
        // Protocols
        DependencyInfo { name: "suppaftp".into(), version: env!("DEP_VERSION_SUPPAFTP").into(), category: "Protocols".into() },
        DependencyInfo { name: "russh".into(), version: env!("DEP_VERSION_RUSSH").into(), category: "Protocols".into() },
        DependencyInfo { name: "russh-sftp".into(), version: env!("DEP_VERSION_RUSSH_SFTP").into(), category: "Protocols".into() },
        DependencyInfo { name: "reqwest".into(), version: env!("DEP_VERSION_REQWEST").into(), category: "Protocols".into() },
        DependencyInfo { name: "quick-xml".into(), version: env!("DEP_VERSION_QUICK_XML").into(), category: "Protocols".into() },
        DependencyInfo { name: "oauth2".into(), version: env!("DEP_VERSION_OAUTH2").into(), category: "Protocols".into() },
        // Security
        DependencyInfo { name: "keyring".into(), version: env!("DEP_VERSION_KEYRING").into(), category: "Security".into() },
        DependencyInfo { name: "argon2".into(), version: env!("DEP_VERSION_ARGON2").into(), category: "Security".into() },
        DependencyInfo { name: "aes-gcm".into(), version: env!("DEP_VERSION_AES_GCM").into(), category: "Security".into() },
        DependencyInfo { name: "ring".into(), version: env!("DEP_VERSION_RING").into(), category: "Security".into() },
        DependencyInfo { name: "zeroize".into(), version: env!("DEP_VERSION_ZEROIZE").into(), category: "Security".into() },
        DependencyInfo { name: "secrecy".into(), version: env!("DEP_VERSION_SECRECY").into(), category: "Security".into() },
        DependencyInfo { name: "sha2".into(), version: env!("DEP_VERSION_SHA2").into(), category: "Security".into() },
        DependencyInfo { name: "hmac".into(), version: env!("DEP_VERSION_HMAC").into(), category: "Security".into() },
        // Archives
        DependencyInfo { name: "sevenz-rust".into(), version: env!("DEP_VERSION_SEVENZ_RUST").into(), category: "Archives".into() },
        DependencyInfo { name: "zip".into(), version: env!("DEP_VERSION_ZIP").into(), category: "Archives".into() },
        DependencyInfo { name: "tar".into(), version: env!("DEP_VERSION_TAR").into(), category: "Archives".into() },
        DependencyInfo { name: "flate2".into(), version: env!("DEP_VERSION_FLATE2").into(), category: "Archives".into() },
        DependencyInfo { name: "xz2".into(), version: env!("DEP_VERSION_XZ2").into(), category: "Archives".into() },
        DependencyInfo { name: "bzip2".into(), version: env!("DEP_VERSION_BZIP2").into(), category: "Archives".into() },
        // Tauri Plugins
        DependencyInfo { name: "tauri-plugin-fs".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_FS").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-dialog".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_DIALOG").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-shell".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_SHELL").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-notification".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_NOTIFICATION").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-log".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_LOG").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-single-instance".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_SINGLE_INSTANCE").into(), category: "Plugins".into() },
    ]
}

#[tauri::command]
async fn check_crate_versions(crate_names: Vec<String>) -> Vec<CrateVersionResult> {
    let client = reqwest::Client::builder()
        .user_agent("AeroFTP (https://github.com/axpnet/aeroftp)")
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut results = Vec::new();
    for chunk in crate_names.chunks(5) {
        let mut handles = Vec::new();
        for name in chunk {
            let client = client.clone();
            let name = name.clone();
            handles.push(tokio::spawn(async move {
                match client
                    .get(format!("https://crates.io/api/v1/crates/{}", name))
                    .send()
                    .await
                {
                    Ok(res) if res.status().is_success() => {
                        match res.json::<serde_json::Value>().await {
                            Ok(data) => {
                                // Prefer max_stable_version to skip pre-releases (beta, rc, alpha)
                                let version = data["crate"]["max_stable_version"]
                                    .as_str()
                                    .or_else(|| data["crate"]["newest_version"].as_str())
                                    .or_else(|| data["crate"]["max_version"].as_str())
                                    .map(|s| s.to_string());
                                CrateVersionResult {
                                    name,
                                    latest_version: version,
                                    error: None,
                                }
                            }
                            Err(e) => CrateVersionResult {
                                name,
                                latest_version: None,
                                error: Some(format!("Parse error: {}", e)),
                            },
                        }
                    }
                    Ok(res) => CrateVersionResult {
                        name,
                        latest_version: None,
                        error: Some(format!("HTTP {}", res.status())),
                    },
                    Err(e) => CrateVersionResult {
                        name,
                        latest_version: None,
                        error: Some(format!("{}", e)),
                    },
                }
            }));
        }
        for handle in handles {
            if let Ok(result) = handle.await {
                results.push(result);
            }
        }
        // Small delay between batches
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    results
}

#[tauri::command]
fn get_system_info() -> SystemInfo {
    let config_dir = dirs::config_dir()
        .map(|d| d.join("aeroftp").to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());

    let vault_exists = dirs::config_dir()
        .map(|d| d.join("aeroftp").join("vault.db").exists())
        .unwrap_or(false);

    let known_hosts_exists = dirs::home_dir()
        .map(|d| d.join(".ssh").join("known_hosts").exists())
        .unwrap_or(false);

    let keyring_backend = if cfg!(target_os = "linux") {
        "gnome-keyring / Secret Service"
    } else if cfg!(target_os = "macos") {
        "macOS Keychain"
    } else if cfg!(target_os = "windows") {
        "Windows Credential Manager"
    } else {
        "unknown"
    };

    let mut dep_versions = std::collections::HashMap::new();
    dep_versions.insert("russh".into(), env!("DEP_VERSION_RUSSH").into());
    dep_versions.insert("russh-sftp".into(), env!("DEP_VERSION_RUSSH_SFTP").into());
    dep_versions.insert("suppaftp".into(), env!("DEP_VERSION_SUPPAFTP").into());
    dep_versions.insert("reqwest".into(), env!("DEP_VERSION_REQWEST").into());
    dep_versions.insert("keyring".into(), env!("DEP_VERSION_KEYRING").into());
    dep_versions.insert("aes-gcm".into(), env!("DEP_VERSION_AES_GCM").into());
    dep_versions.insert("argon2".into(), env!("DEP_VERSION_ARGON2").into());
    dep_versions.insert("zip".into(), env!("DEP_VERSION_ZIP").into());
    dep_versions.insert("sevenz-rust".into(), env!("DEP_VERSION_SEVENZ_RUST").into());
    dep_versions.insert("quick-xml".into(), env!("DEP_VERSION_QUICK_XML").into());
    dep_versions.insert("oauth2".into(), env!("DEP_VERSION_OAUTH2").into());

    SystemInfo {
        app_version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        os_version: std::env::consts::ARCH.into(),
        arch: std::env::consts::ARCH.into(),
        tauri_version: env!("DEP_VERSION_TAURI").into(),
        rust_version: "1.77.2+".into(),
        keyring_backend: keyring_backend.into(),
        config_dir,
        vault_exists,
        known_hosts_exists,
        dep_versions,
    }
}

// ============ Local File System Commands ============

#[tauri::command]
async fn get_local_files(path: String, show_hidden: Option<bool>) -> Result<Vec<LocalFileInfo>, String> {
    let path = PathBuf::from(&path);
    let show_hidden = show_hidden.unwrap_or(true);  // Developer-first: show all files by default
    
    if !path.exists() {
        return Err(format!("Path does not exist: {}", path.display()));
    }

    let mut files = Vec::new();
    
    // Parent directory (..) removed - use "Up" button in toolbar for navigation

    let mut entries = tokio::fs::read_dir(&path)
        .await
        .map_err(|e| format!("Failed to read directory: {}", e))?;

    while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
        let metadata = entry.metadata().await.ok();
        let file_name = entry.file_name().to_string_lossy().to_string();
        
        // Skip hidden files unless show_hidden is enabled
        if !show_hidden && file_name.starts_with('.') {
            continue;
        }

        let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = if is_dir { 
            None 
        } else { 
            metadata.as_ref().map(|m| m.len()) 
        };
        
        let modified = metadata.as_ref().and_then(|m| {
            m.modified().ok().map(|t| {
                let datetime: chrono::DateTime<chrono::Local> = t.into();
                datetime.format("%Y-%m-%d %H:%M").to_string()
            })
        });

        files.push(LocalFileInfo {
            name: file_name,
            path: entry.path().to_string_lossy().to_string(),
            size,
            is_dir,
            modified,
        });
    }

    // Sort: directories first, then alphabetically
    files.sort_by(|a, b| {
        match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    });

    Ok(files)
}

#[tauri::command]
async fn open_in_file_manager(path: String) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open file manager: {}", e))?;
    }
    
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open file manager: {}", e))?;
    }
    
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open file manager: {}", e))?;
    }
    
    Ok(())
}

// ============ File Operations Commands ============

/// Delete a remote file or folder with detailed event emission for each deleted item.
/// For folders, recursively scans and emits events for each file deleted.
#[tauri::command]
async fn delete_remote_file(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    is_dir: bool
) -> Result<String, String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    
    let file_name = path.split('/').last().unwrap_or(&path).to_string();
    let delete_id = format!("del-remote-{}", chrono::Utc::now().timestamp_millis());
    
    if !is_dir {
        // Single file delete - simple case
        let _ = app.emit("transfer_event", TransferEvent {
            event_type: "delete_start".to_string(),
            transfer_id: delete_id.clone(),
            filename: file_name.clone(),
            direction: "remote".to_string(),
            message: Some(format!("Deleting remote file: {}", file_name)),
            progress: None,
        });
        
        match ftp_manager.remove(&path).await {
            Ok(_) => {
                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "delete_complete".to_string(),
                    transfer_id: delete_id.clone(),
                    filename: file_name.clone(),
                    direction: "remote".to_string(),
                    message: Some(format!("Deleted remote file: {}", file_name)),
                    progress: None,
                });
                Ok(format!("Deleted: {}", file_name))
            }
            Err(e) => {
                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "delete_error".to_string(),
                    transfer_id: delete_id.clone(),
                    filename: file_name.clone(),
                    direction: "remote".to_string(),
                    message: Some(format!("Failed to delete: {}", e)),
                    progress: None,
                });
                Err(format!("Failed to delete file: {}", e))
            }
        }
    } else {
        // Folder delete - scan first, then delete with events
        let _ = app.emit("transfer_event", TransferEvent {
            event_type: "delete_start".to_string(),
            transfer_id: delete_id.clone(),
            filename: file_name.clone(),
            direction: "remote".to_string(),
            message: Some(format!("Scanning remote folder: {}", file_name)),
            progress: None,
        });
        
        let original_path = ftp_manager.current_path();
        
        // Build absolute target path
        let target_path = if path.starts_with('/') {
            path.clone()
        } else {
            format!("{}/{}", original_path, path)
        };
        
        // Phase 1: Collect all files and directories recursively
        struct DeleteItem {
            path: String,
            name: String,
        }
        
        let mut files_to_delete: Vec<DeleteItem> = Vec::new();
        let mut dirs_to_delete: Vec<String> = Vec::new();
        let mut dirs_to_scan: Vec<String> = vec![target_path.clone()];
        
        while let Some(current_dir) = dirs_to_scan.pop() {
            if ftp_manager.change_dir(&current_dir).await.is_err() {
                continue;
            }
            
            let files = match ftp_manager.list_files().await {
                Ok(f) => f,
                Err(_) => continue,
            };
            
            for file in files {
                let file_path = format!("{}/{}", current_dir, file.name);
                
                if file.is_dir {
                    dirs_to_scan.push(file_path.clone());
                } else {
                    files_to_delete.push(DeleteItem {
                        path: file_path,
                        name: file.name,
                    });
                }
            }
            
            // Add directory to delete list (will be deleted after its contents)
            dirs_to_delete.push(current_dir);
        }
        
        let total_files = files_to_delete.len();
        let total_dirs = dirs_to_delete.len();
        
        info!("Found {} files and {} directories to delete in {}", total_files, total_dirs, file_name);
        
        let _ = app.emit("transfer_event", TransferEvent {
            event_type: "progress".to_string(),
            transfer_id: delete_id.clone(),
            filename: file_name.clone(),
            direction: "remote".to_string(),
            message: Some(format!("Found {} files in {} folders to delete", total_files, total_dirs)),
            progress: None,
        });
        
        // Phase 2: Delete all files with events
        let mut deleted_files = 0u64;
        let mut errors = 0u64;
        
        for item in &files_to_delete {
            let file_delete_id = format!("{}-file-{}", delete_id, deleted_files);
            
            let _ = app.emit("transfer_event", TransferEvent {
                event_type: "delete_file_start".to_string(),
                transfer_id: file_delete_id.clone(),
                filename: item.name.clone(),
                direction: "remote".to_string(),
                message: Some(format!("Deleting: {}", item.path)),
                progress: None,
            });
            
            match ftp_manager.remove(&item.path).await {
                Ok(_) => {
                    deleted_files += 1;
                    let _ = app.emit("transfer_event", TransferEvent {
                        event_type: "delete_file_complete".to_string(),
                        transfer_id: file_delete_id,
                        filename: item.name.clone(),
                        direction: "remote".to_string(),
                        message: Some(format!("Deleted: {}", item.name)),
                        progress: None,
                    });
                }
                Err(e) => {
                    errors += 1;
                    warn!("Failed to delete {}: {}", item.path, e);
                    let _ = app.emit("transfer_event", TransferEvent {
                        event_type: "delete_file_error".to_string(),
                        transfer_id: file_delete_id,
                        filename: item.name.clone(),
                        direction: "remote".to_string(),
                        message: Some(format!("Failed: {} - {}", item.name, e)),
                        progress: None,
                    });
                }
            }
        }
        
        // Phase 3: Delete directories (deepest first - reverse the order!)
        // Directories were added in scan order (parent first), so we need to reverse
        let dirs_reversed: Vec<_> = dirs_to_delete.iter().rev().collect();
        for dir_path in dirs_reversed {
            let dir_name = dir_path.split('/').last().unwrap_or(dir_path);
            match ftp_manager.remove_dir(dir_path).await {
                Ok(_) => {
                    let _ = app.emit("transfer_event", TransferEvent {
                        event_type: "delete_dir_complete".to_string(),
                        transfer_id: delete_id.clone(),
                        filename: dir_name.to_string(),
                        direction: "remote".to_string(),
                        message: Some(format!("Removed folder: {}", dir_name)),
                        progress: None,
                    });
                }
                Err(e) => {
                    warn!("Failed to remove remote directory {}: {}", dir_path, e);
                }
            }
        }
        
        // Return to original directory
        let _ = ftp_manager.change_dir(&original_path).await;
        
        // Emit completion
        let result_message = if errors > 0 {
            format!("Deleted {} files ({} errors), {} folders", deleted_files, errors, total_dirs)
        } else {
            format!("Deleted {} files, {} folders", deleted_files, total_dirs)
        };
        
        let _ = app.emit("transfer_event", TransferEvent {
            event_type: "delete_complete".to_string(),
            transfer_id: delete_id.clone(),
            filename: file_name.clone(),
            direction: "remote".to_string(),
            message: Some(result_message.clone()),
            progress: None,
        });
        
        Ok(result_message)
    }
}

/// Delete a local file or folder with detailed event emission for each deleted item.
#[tauri::command]
async fn delete_local_file(app: AppHandle, path: String) -> Result<String, String> {
    let path_buf = std::path::PathBuf::from(&path);
    let file_name = path_buf.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    
    let delete_id = format!("del-local-{}", chrono::Utc::now().timestamp_millis());
    let is_dir = path_buf.is_dir();
    
    if !is_dir {
        // Single file delete
        let _ = app.emit("transfer_event", TransferEvent {
            event_type: "delete_start".to_string(),
            transfer_id: delete_id.clone(),
            filename: file_name.clone(),
            direction: "local".to_string(),
            message: Some(format!("Deleting local file: {}", file_name)),
            progress: None,
        });
        
        match tokio::fs::remove_file(&path).await {
            Ok(_) => {
                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "delete_complete".to_string(),
                    transfer_id: delete_id.clone(),
                    filename: file_name.clone(),
                    direction: "local".to_string(),
                    message: Some(format!("Deleted local file: {}", file_name)),
                    progress: None,
                });
                Ok(format!("Deleted: {}", file_name))
            }
            Err(e) => {
                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "delete_error".to_string(),
                    transfer_id: delete_id.clone(),
                    filename: file_name.clone(),
                    direction: "local".to_string(),
                    message: Some(format!("Failed to delete: {}", e)),
                    progress: None,
                });
                Err(format!("Failed to delete file: {}", e))
            }
        }
    } else {
        // Folder delete - scan first, then delete with events
        let _ = app.emit("transfer_event", TransferEvent {
            event_type: "delete_start".to_string(),
            transfer_id: delete_id.clone(),
            filename: file_name.clone(),
            direction: "local".to_string(),
            message: Some(format!("Scanning local folder: {}", file_name)),
            progress: None,
        });
        
        // Phase 1: Collect all files and directories
        struct DeleteItem {
            path: std::path::PathBuf,
            name: String,
        }
        
        let mut files_to_delete: Vec<DeleteItem> = Vec::new();
        let mut dirs_to_delete: Vec<std::path::PathBuf> = Vec::new();
        let mut dirs_to_scan: Vec<std::path::PathBuf> = vec![path_buf.clone()];
        
        while let Some(current_dir) = dirs_to_scan.pop() {
            let mut read_dir = match tokio::fs::read_dir(&current_dir).await {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            
            while let Ok(Some(entry)) = read_dir.next_entry().await {
                let entry_path = entry.path();
                let entry_name = entry.file_name().to_string_lossy().to_string();
                
                if entry_path.is_dir() {
                    dirs_to_scan.push(entry_path.clone());
                } else {
                    files_to_delete.push(DeleteItem {
                        path: entry_path,
                        name: entry_name,
                    });
                }
            }
            
            dirs_to_delete.push(current_dir);
        }
        
        let total_files = files_to_delete.len();
        let total_dirs = dirs_to_delete.len();
        
        info!("Found {} files and {} directories to delete in {}", total_files, total_dirs, file_name);
        
        let _ = app.emit("transfer_event", TransferEvent {
            event_type: "progress".to_string(),
            transfer_id: delete_id.clone(),
            filename: file_name.clone(),
            direction: "local".to_string(),
            message: Some(format!("Found {} files in {} folders to delete", total_files, total_dirs)),
            progress: None,
        });
        
        // Phase 2: Delete all files with events
        let mut deleted_files = 0u64;
        let mut errors = 0u64;
        let mut last_emit = std::time::Instant::now();
        
        for item in &files_to_delete {
            match tokio::fs::remove_file(&item.path).await {
                Ok(_) => {
                    deleted_files += 1;
                    
                    // Emit progress every 100ms or every 50 files to avoid flooding
                    if last_emit.elapsed().as_millis() > 100 || deleted_files % 50 == 0 || deleted_files == total_files as u64 {
                        let _ = app.emit("transfer_event", TransferEvent {
                            event_type: "delete_file_complete".to_string(),
                            transfer_id: delete_id.clone(),
                            filename: item.name.clone(),
                            direction: "local".to_string(),
                            message: Some(format!("Deleted [{}/{}]: {}", deleted_files, total_files, item.name)),
                            progress: None,
                        });
                        last_emit = std::time::Instant::now();
                    }
                }
                Err(e) => {
                    errors += 1;
                    let _ = app.emit("transfer_event", TransferEvent {
                        event_type: "delete_file_error".to_string(),
                        transfer_id: delete_id.clone(),
                        filename: item.name.clone(),
                        direction: "local".to_string(),
                        message: Some(format!("Failed: {} - {}", item.name, e)),
                        progress: None,
                    });
                }
            }
        }
        
        // Phase 3: Delete directories (deepest first - reverse the order!)
        // Directories were added in scan order (parent first), so we need to reverse
        // to delete children before parents
        let dirs_reversed: Vec<_> = dirs_to_delete.iter().rev().collect();
        for dir_path in dirs_reversed {
            let dir_name = dir_path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "folder".to_string());
            
            match tokio::fs::remove_dir(dir_path).await {
                Ok(_) => {
                    let _ = app.emit("transfer_event", TransferEvent {
                        event_type: "delete_dir_complete".to_string(),
                        transfer_id: delete_id.clone(),
                        filename: dir_name,
                        direction: "local".to_string(),
                        message: Some(format!("Removed folder: {}", dir_path.display())),
                        progress: None,
                    });
                }
                Err(e) => {
                    warn!("Failed to remove directory {:?}: {}", dir_path, e);
                }
            }
        }
        
        // Emit completion
        let result_message = if errors > 0 {
            format!("Deleted {} files ({} errors), {} folders", deleted_files, errors, total_dirs)
        } else {
            format!("Deleted {} files, {} folders", deleted_files, total_dirs)
        };
        
        let _ = app.emit("transfer_event", TransferEvent {
            event_type: "delete_complete".to_string(),
            transfer_id: delete_id.clone(),
            filename: file_name.clone(),
            direction: "local".to_string(),
            message: Some(result_message.clone()),
            progress: None,
        });
        
        Ok(result_message)
    }
}

#[tauri::command]
async fn rename_remote_file(state: State<'_, AppState>, from: String, to: String) -> Result<(), String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    
    ftp_manager.rename(&from, &to)
        .await
        .map_err(|e| format!("Failed to rename: {}", e))?;
    
    Ok(())
}

#[tauri::command]
async fn create_remote_folder(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    
    ftp_manager.mkdir(&path)
        .await
        .map_err(|e| format!("Failed to create folder: {}", e))?;
    
    Ok(())
}

#[tauri::command]
async fn chmod_remote_file(state: State<'_, AppState>, path: String, mode: String) -> Result<(), String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    ftp_manager.chmod(&path, &mode)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn rename_local_file(from: String, to: String) -> Result<(), String> {
    tokio::fs::rename(&from, &to)
        .await
        .map_err(|e| format!("Failed to rename: {}", e))?;
    
    Ok(())
}

#[tauri::command]
async fn create_local_folder(path: String) -> Result<(), String> {
    tokio::fs::create_dir_all(&path)
        .await
        .map_err(|e| format!("Failed to create folder: {}", e))?;
    
    Ok(())
}

#[tauri::command]
async fn read_file_base64(path: String) -> Result<String, String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let data = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;

    Ok(STANDARD.encode(data))
}

/// Calculate checksum (MD5 or SHA-256) for a local file
#[tauri::command]
async fn calculate_checksum(path: String, algorithm: String) -> Result<String, String> {
    use md5::Md5;
    use sha2::{Sha256, Digest};
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| format!("Failed to open file: {}", e))?;

    match algorithm.to_lowercase().as_str() {
        "md5" => {
            let mut hasher = Md5::new();
            let mut buffer = vec![0u8; 64 * 1024]; // 64KB buffer

            loop {
                let bytes_read = file.read(&mut buffer).await
                    .map_err(|e| format!("Failed to read file: {}", e))?;
                if bytes_read == 0 { break; }
                hasher.update(&buffer[..bytes_read]);
            }

            let result = hasher.finalize();
            Ok(hex::encode(result))
        }
        "sha256" => {
            let mut hasher = Sha256::new();
            let mut buffer = vec![0u8; 64 * 1024]; // 64KB buffer

            loop {
                let bytes_read = file.read(&mut buffer).await
                    .map_err(|e| format!("Failed to read file: {}", e))?;
                if bytes_read == 0 { break; }
                hasher.update(&buffer[..bytes_read]);
            }

            let result = hasher.finalize();
            Ok(hex::encode(result))
        }
        _ => Err(format!("Unsupported algorithm: {}. Use 'md5' or 'sha256'", algorithm))
    }
}

/// Compress files/folders into a ZIP archive
#[tauri::command]
async fn compress_files(paths: Vec<String>, output_path: String, password: Option<String>, compression_level: Option<i64>) -> Result<String, String> {
    use std::fs::File;
    use std::io::{Read, Write};
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;
    use walkdir::WalkDir;

    // Wrap password in SecretString for zeroization on drop
    let secret_password: Option<SecretString> = password.map(SecretString::from);

    let file = File::create(&output_path)
        .map_err(|e| format!("Failed to create ZIP file: {}", e))?;

    let mut zip = ZipWriter::new(file);
    let level = compression_level.unwrap_or(6);
    let method = if level == 0 { zip::CompressionMethod::Stored } else { zip::CompressionMethod::Deflated };
    let base_options = SimpleFileOptions::default()
        .compression_method(method)
        .compression_level(Some(level));

    for path in &paths {
        let path = std::path::Path::new(path);

        if path.is_file() {
            let file_name = path.file_name()
                .ok_or("Invalid file name")?
                .to_string_lossy();

            if let Some(ref pwd) = secret_password {
                zip.start_file(file_name.to_string(), base_options.with_aes_encryption(zip::AesMode::Aes256, pwd.expose_secret()))
                    .map_err(|e| format!("Failed to add file to ZIP: {}", e))?;
            } else {
                zip.start_file(file_name.to_string(), base_options)
                    .map_err(|e| format!("Failed to add file to ZIP: {}", e))?;
            }

            let mut f = File::open(path)
                .map_err(|e| format!("Failed to open file: {}", e))?;
            let mut buffer = Vec::new();
            f.read_to_end(&mut buffer)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            zip.write_all(&buffer)
                .map_err(|e| format!("Failed to write to ZIP: {}", e))?;

        } else if path.is_dir() {
            let _base_name = path.file_name()
                .ok_or("Invalid directory name")?
                .to_string_lossy();

            for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
                let entry_path = entry.path();
                let relative_path = entry_path.strip_prefix(path.parent().unwrap_or(path))
                    .map_err(|e| format!("Path error: {}", e))?;

                if entry_path.is_file() {
                    if let Some(ref pwd) = secret_password {
                        zip.start_file(relative_path.to_string_lossy().to_string(), base_options.with_aes_encryption(zip::AesMode::Aes256, pwd.expose_secret()))
                            .map_err(|e| format!("Failed to add file to ZIP: {}", e))?;
                    } else {
                        zip.start_file(relative_path.to_string_lossy().to_string(), base_options)
                            .map_err(|e| format!("Failed to add file to ZIP: {}", e))?;
                    }

                    let mut f = File::open(entry_path)
                        .map_err(|e| format!("Failed to open file: {}", e))?;
                    let mut buffer = Vec::new();
                    f.read_to_end(&mut buffer)
                        .map_err(|e| format!("Failed to read file: {}", e))?;
                    zip.write_all(&buffer)
                        .map_err(|e| format!("Failed to write to ZIP: {}", e))?;

                } else if entry_path.is_dir() && entry_path != path {
                    let dir_path = format!("{}/", relative_path.to_string_lossy());
                    if let Some(ref pwd) = secret_password {
                        zip.add_directory(&dir_path, base_options.with_aes_encryption(zip::AesMode::Aes256, pwd.expose_secret()))
                            .map_err(|e| format!("Failed to add directory to ZIP: {}", e))?;
                    } else {
                        zip.add_directory(&dir_path, base_options)
                            .map_err(|e| format!("Failed to add directory to ZIP: {}", e))?;
                    }
                }
            }
        }
    }

    zip.finish()
        .map_err(|e| format!("Failed to finalize ZIP: {}", e))?;

    Ok(output_path)
}

/// Extract a ZIP archive
#[tauri::command]
async fn extract_archive(archive_path: String, output_dir: String, create_subfolder: bool, password: Option<String>) -> Result<String, String> {
    use std::fs::{self, File};
    use zip::ZipArchive;

    // Wrap password in SecretString for zeroization on drop
    let secret_password: Option<SecretString> = password.map(SecretString::from);

    let file = File::open(&archive_path)
        .map_err(|e| format!("Failed to open archive: {}", e))?;

    let mut archive = ZipArchive::new(file)
        .map_err(|e| format!("Failed to read archive: {}", e))?;

    // Determine actual output directory
    let actual_output = if create_subfolder {
        let archive_stem = std::path::Path::new(&archive_path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let subfolder = std::path::Path::new(&output_dir).join(&archive_stem);
        subfolder.to_string_lossy().to_string()
    } else {
        output_dir.clone()
    };

    // Create output directory if needed
    fs::create_dir_all(&actual_output)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    for i in 0..archive.len() {
        let mut file = if let Some(ref pwd) = secret_password {
            archive.by_index_decrypt(i, pwd.expose_secret().as_bytes())
                .map_err(|e| format!("Failed to decrypt file from archive: {}", e))?
        } else {
            archive.by_index(i)
                .map_err(|e| format!("Failed to read file from archive: {}", e))?
        };

        let outpath = std::path::Path::new(&actual_output).join(file.name());

        if file.name().ends_with('/') {
            // Directory
            fs::create_dir_all(&outpath)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        } else {
            // File
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent directory: {}", e))?;
            }

            let mut outfile = File::create(&outpath)
                .map_err(|e| format!("Failed to create file: {}", e))?;

            std::io::copy(&mut file, &mut outfile)
                .map_err(|e| format!("Failed to extract file: {}", e))?;
        }
    }

    Ok(actual_output)
}

/// Compress files/folders into a 7z archive (LZMA2 compression)
#[tauri::command]
async fn compress_7z(
    paths: Vec<String>,
    output_path: String,
    password: Option<String>,
    _compression_level: Option<i64>,
) -> Result<String, String> {
    use sevenz_rust::*;
    use std::fs::File;
    use std::path::Path;
    use walkdir::WalkDir;

    // Wrap password in SecretString for zeroization on drop
    let secret_password: Option<SecretString> = password.map(SecretString::from);

    // Collect all files to compress
    let mut entries: Vec<(String, String)> = Vec::new(); // (archive_name, full_path)

    for path_str in &paths {
        let path = Path::new(path_str);

        if path.is_file() {
            let file_name = path.file_name()
                .ok_or("Invalid file name")?
                .to_string_lossy()
                .to_string();
            entries.push((file_name, path_str.clone()));
        } else if path.is_dir() {
            // Add directory contents recursively
            for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
                let entry_path = entry.path();
                if entry_path.is_file() {
                    let relative_path = entry_path
                        .strip_prefix(path.parent().unwrap_or(path))
                        .map_err(|e| format!("Path error: {}", e))?;
                    entries.push((
                        relative_path.to_string_lossy().to_string(),
                        entry_path.to_string_lossy().to_string(),
                    ));
                }
            }
        }
    }

    if entries.is_empty() {
        return Err("No files to compress".to_string());
    }

    // Create the 7z archive
    let output_file = File::create(&output_path)
        .map_err(|e| format!("Failed to create 7z file: {}", e))?;

    let mut sz = SevenZWriter::new(output_file)
        .map_err(|e| format!("Failed to create 7z writer: {}", e))?;

    // Set compression and optional AES-256 encryption
    if let Some(ref pwd) = secret_password {
        let aes_options = AesEncoderOptions::new(Password::from(pwd.expose_secret()));
        sz.set_content_methods(vec![
            aes_options.into(),
            SevenZMethodConfiguration::new(SevenZMethod::LZMA2),
        ]);
    } else {
        sz.set_content_methods(vec![
            SevenZMethodConfiguration::new(SevenZMethod::LZMA2),
        ]);
    }

    // Add files to archive
    for (archive_name, full_path) in &entries {
        let source_path = Path::new(full_path);
        let entry = SevenZArchiveEntry::from_path(source_path, archive_name.clone());

        // Open file and create reader
        let file = File::open(source_path)
            .map_err(|e| format!("Failed to open file '{}': {}", archive_name, e))?;

        sz.push_archive_entry(entry, Some(file))
            .map_err(|e| format!("Failed to add file '{}': {}", archive_name, e))?;
    }

    sz.finish()
        .map_err(|e| format!("Failed to finalize 7z archive: {}", e))?;

    Ok(output_path)
}

/// Extract a 7z archive with optional password (AES-256 decryption)
#[tauri::command]
async fn extract_7z(
    archive_path: String,
    output_dir: String,
    password: Option<String>,
    create_subfolder: bool,
) -> Result<String, String> {
    use sevenz_rust::*;
    use std::fs;
    use std::path::Path;

    // Determine output directory
    let final_output_dir = if create_subfolder {
        let archive_name = Path::new(&archive_path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "extracted".to_string());
        Path::new(&output_dir).join(&archive_name).to_string_lossy().to_string()
    } else {
        output_dir.clone()
    };

    // Create output directory
    fs::create_dir_all(&final_output_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    // Wrap password in SecretString for zeroization on drop
    let secret_password: Option<SecretString> = password.map(SecretString::from);

    // Extract archive
    if let Some(ref pwd) = secret_password {
        decompress_file_with_password(
            &archive_path,
            &final_output_dir,
            Password::from(pwd.expose_secret()),
        ).map_err(|e| format!("Failed to extract 7z archive: {}", e))?;
    } else {
        decompress_file(&archive_path, &final_output_dir)
            .map_err(|e| format!("Failed to extract 7z archive: {}", e))?;
    }

    Ok(final_output_dir)
}

/// Check if a 7z archive is password protected
#[tauri::command]
async fn is_7z_encrypted(archive_path: String) -> Result<bool, String> {
    use sevenz_rust::*;
    use std::fs::File;
    use std::io::BufReader;

    let file = File::open(&archive_path)
        .map_err(|e| format!("Failed to open archive: {}", e))?;

    let len = file.metadata()
        .map_err(|e| format!("Failed to get file metadata: {}", e))?
        .len();

    let reader = BufReader::new(file);

    // Try to open without password — 7z metadata is often unencrypted even when content is
    let mut archive = match SevenZReader::new(reader, len, Password::empty()) {
        Ok(a) => a,
        Err(e) => {
            let err_str = format!("{:?}", e);
            if err_str.contains("password") || err_str.contains("Password") || err_str.contains("encrypted") || err_str.contains("Encrypted") {
                return Ok(true);
            }
            return Ok(false);
        }
    };

    // Metadata opened fine, but content may still be encrypted.
    // Try to decompress the first file — if it fails, content is encrypted.
    let has_files = archive.archive().files.iter().any(|f| f.has_stream());
    if !has_files {
        return Ok(false);
    }

    let mut encrypted = false;
    let result = archive.for_each_entries(|_entry, reader| {
        let mut buf = [0u8; 1];
        match reader.read(&mut buf) {
            Ok(_) => {}
            Err(_) => { encrypted = true; }
        }
        // Stop after first entry
        Ok(false)
    });

    if result.is_err() {
        encrypted = true;
    }

    Ok(encrypted)
}

/// Check if a ZIP archive is password protected (AES or ZipCrypto)
#[tauri::command]
async fn is_zip_encrypted(archive_path: String) -> Result<bool, String> {
    use std::fs::File;
    use zip::ZipArchive;

    let file = File::open(&archive_path)
        .map_err(|e| format!("Failed to open archive: {}", e))?;

    let mut archive = ZipArchive::new(file)
        .map_err(|e| format!("Failed to read archive: {}", e))?;

    // Check if any file in the archive is encrypted
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index_raw(i) {
            if entry.encrypted() {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Compress files/folders into a TAR-based archive.
/// Supports formats: "tar", "tar.gz", "tar.xz", "tar.bz2"
#[tauri::command]
async fn compress_tar(
    paths: Vec<String>,
    output_path: String,
    format: String,
    compression_level: Option<i64>,
) -> Result<String, String> {
    use std::fs::File;
    use std::path::Path;
    use walkdir::WalkDir;

    let output = Path::new(&output_path);

    // Collect all files (expanding directories recursively)
    let mut entries: Vec<(std::path::PathBuf, String)> = Vec::new();
    for p in &paths {
        let path = Path::new(p);
        if path.is_dir() {
            for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    let rel = entry.path().strip_prefix(path.parent().unwrap_or(path))
                        .unwrap_or(entry.path());
                    entries.push((entry.path().to_path_buf(), rel.to_string_lossy().to_string()));
                }
            }
            // Directory entries are created automatically by tar when adding files
        } else if path.is_file() {
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            entries.push((path.to_path_buf(), name));
        }
    }

    if entries.is_empty() {
        return Err("No files to compress".to_string());
    }

    // Create the archive based on format
    let file = File::create(output).map_err(|e| format!("Failed to create archive: {}", e))?;

    match format.as_str() {
        "tar" => {
            let mut archive = tar::Builder::new(file);
            for (abs_path, rel_path) in &entries {
                archive.append_path_with_name(abs_path, rel_path)
                    .map_err(|e| format!("Failed to add {}: {}", rel_path, e))?;
            }
            archive.finish().map_err(|e| format!("Failed to finalize tar: {}", e))?;
        }
        "tar.gz" => {
            let gz = flate2::write::GzEncoder::new(file, flate2::Compression::new(compression_level.unwrap_or(6) as u32));
            let mut archive = tar::Builder::new(gz);
            for (abs_path, rel_path) in &entries {
                archive.append_path_with_name(abs_path, rel_path)
                    .map_err(|e| format!("Failed to add {}: {}", rel_path, e))?;
            }
            archive.into_inner().map_err(|e| format!("Failed to finalize gz: {}", e))?
                .finish().map_err(|e| format!("Failed to finish gz: {}", e))?;
        }
        "tar.xz" => {
            let xz = xz2::write::XzEncoder::new(file, compression_level.unwrap_or(6) as u32);
            let mut archive = tar::Builder::new(xz);
            for (abs_path, rel_path) in &entries {
                archive.append_path_with_name(abs_path, rel_path)
                    .map_err(|e| format!("Failed to add {}: {}", rel_path, e))?;
            }
            archive.into_inner().map_err(|e| format!("Failed to finalize xz: {}", e))?
                .finish().map_err(|e| format!("Failed to finish xz: {}", e))?;
        }
        "tar.bz2" => {
            let bz = bzip2::write::BzEncoder::new(file, bzip2::Compression::new(compression_level.unwrap_or(6) as u32));
            let mut archive = tar::Builder::new(bz);
            for (abs_path, rel_path) in &entries {
                archive.append_path_with_name(abs_path, rel_path)
                    .map_err(|e| format!("Failed to add {}: {}", rel_path, e))?;
            }
            archive.into_inner().map_err(|e| format!("Failed to finalize bz2: {}", e))?
                .finish().map_err(|e| format!("Failed to finish bz2: {}", e))?;
        }
        _ => return Err(format!("Unsupported format: {}", format)),
    }

    let file_count = entries.len();
    Ok(format!("Compressed {} files into {}", file_count, output.display()))
}

/// Extract TAR-based archives (auto-detects tar, tar.gz, tar.xz, tar.bz2 from extension)
#[tauri::command]
async fn extract_tar(
    archive_path: String,
    output_dir: String,
    create_subfolder: bool,
) -> Result<String, String> {
    use std::fs::File;
    use std::path::Path;

    let archive = Path::new(&archive_path);
    let out = Path::new(&output_dir);

    // Determine subfolder name from archive filename
    let final_output = if create_subfolder {
        let stem = archive.file_stem().unwrap_or_default().to_string_lossy();
        // Handle double extensions like .tar.gz -> strip both
        let folder_name = if stem.ends_with(".tar") {
            stem.trim_end_matches(".tar").to_string()
        } else {
            stem.to_string()
        };
        let subfolder = out.join(&folder_name);
        std::fs::create_dir_all(&subfolder).map_err(|e| format!("Failed to create dir: {}", e))?;
        subfolder
    } else {
        out.to_path_buf()
    };

    let file = File::open(archive).map_err(|e| format!("Failed to open archive: {}", e))?;
    let ext = archive.to_string_lossy().to_lowercase();

    if ext.ends_with(".tar.gz") || ext.ends_with(".tgz") {
        let gz = flate2::read::GzDecoder::new(file);
        let mut ar = tar::Archive::new(gz);
        ar.unpack(&final_output).map_err(|e| format!("Failed to extract tar.gz: {}", e))?;
    } else if ext.ends_with(".tar.xz") || ext.ends_with(".txz") {
        let xz = xz2::read::XzDecoder::new(file);
        let mut ar = tar::Archive::new(xz);
        ar.unpack(&final_output).map_err(|e| format!("Failed to extract tar.xz: {}", e))?;
    } else if ext.ends_with(".tar.bz2") || ext.ends_with(".tbz2") {
        let bz = bzip2::read::BzDecoder::new(file);
        let mut ar = tar::Archive::new(bz);
        ar.unpack(&final_output).map_err(|e| format!("Failed to extract tar.bz2: {}", e))?;
    } else if ext.ends_with(".tar") {
        let mut ar = tar::Archive::new(file);
        ar.unpack(&final_output).map_err(|e| format!("Failed to extract tar: {}", e))?;
    } else {
        return Err(format!("Unrecognized archive format: {}", ext));
    }

    Ok(final_output.to_string_lossy().to_string())
}

/// Extract a RAR archive with optional password
#[tauri::command]
async fn extract_rar(
    archive_path: String,
    output_dir: String,
    password: Option<String>,
    create_subfolder: bool,
) -> Result<String, String> {
    use std::path::Path;

    let final_output = if create_subfolder {
        let archive_name = Path::new(&archive_path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "extracted".to_string());
        Path::new(&output_dir).join(&archive_name)
    } else {
        Path::new(&output_dir).to_path_buf()
    };

    std::fs::create_dir_all(&final_output)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    // Wrap password in SecretString for zeroization on drop
    let secret_password: Option<SecretString> = password.map(SecretString::from);

    let archive = if let Some(ref pwd) = secret_password {
        unrar::Archive::with_password(&archive_path, pwd.expose_secret().as_bytes())
    } else {
        unrar::Archive::new(&archive_path)
    };

    let mut archive = archive.open_for_processing()
        .map_err(|e| format!("Failed to open RAR archive: {}", e))?;

    while let Some(header) = archive.read_header()
        .map_err(|e| format!("Failed to read RAR header: {}", e))?
    {
        archive = if header.entry().is_file() {
            header.extract_with_base(&final_output)
                .map_err(|e| format!("Failed to extract RAR entry: {}", e))?
        } else {
            header.skip()
                .map_err(|e| format!("Failed to skip RAR entry: {}", e))?
        };
    }

    Ok(final_output.to_string_lossy().to_string())
}

/// Check if a RAR archive is password protected
#[tauri::command]
async fn is_rar_encrypted(archive_path: String) -> Result<bool, String> {
    let archive = unrar::Archive::new(&archive_path)
        .open_for_listing()
        .map_err(|e| format!("Failed to open RAR archive: {}", e))?;

    for entry in archive {
        match entry {
            Ok(e) => {
                if e.is_encrypted() {
                    return Ok(true);
                }
            }
            Err(_) => return Ok(true), // If listing fails, assume encrypted
        }
    }

    Ok(false)
}

#[tauri::command]
async fn ftp_read_file_base64(state: State<'_, AppState>, path: String) -> Result<String, String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    
    let mut ftp_manager = state.ftp_manager.lock().await;
    
    // Limit size for preview (10MB should be enough for most media files)
    let max_size: u64 = 10 * 1024 * 1024;
    
    // Get file size first
    let file_size = ftp_manager.get_file_size(&path)
        .await
        .unwrap_or(0);
    
    if file_size > max_size {
        return Err(format!("File too large for preview ({:.1} MB). Max: 10 MB", file_size as f64 / 1024.0 / 1024.0));
    }
    
    // Download to memory
    let data = ftp_manager.download_to_bytes(&path)
        .await
        .map_err(|e| format!("Failed to download: {}", e))?;
    
    Ok(STANDARD.encode(data))
}

// ============ DevTools Commands ============

#[tauri::command]
async fn read_local_file(path: String) -> Result<String, String> {
    // Read local file content as UTF-8 string
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;
    
    Ok(content)
}

#[tauri::command]
async fn read_local_file_base64(path: String, max_size_mb: Option<u32>) -> Result<String, String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    
    // Default max size is 50MB for media files (audio/video)
    let max_size: u64 = (max_size_mb.unwrap_or(50) as u64) * 1024 * 1024;
    
    // Check file size first
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("Failed to read file metadata: {}", e))?;
    
    if metadata.len() > max_size {
        return Err(format!(
            "File too large for preview ({:.1} MB). Max: {} MB",
            metadata.len() as f64 / (1024.0 * 1024.0),
            max_size / (1024 * 1024)
        ));
    }
    
    // Read file as binary
    let content = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;
    
    // Encode as base64
    Ok(STANDARD.encode(&content))
}

#[tauri::command]
async fn preview_remote_file(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    
    // Download file content to memory (limit to 1MB for preview)
    let max_size: u64 = 1024 * 1024; // 1MB limit
    
    // Get file size first
    let file_size = ftp_manager.get_file_size(&path)
        .await
        .unwrap_or(0);
    
    if file_size > max_size {
        return Err(format!("File too large for preview ({} KB). Max: 1024 KB", file_size / 1024));
    }
    
    // Download to temp and read
    let temp_path = std::env::temp_dir().join(format!("aeroftp_preview_{}", chrono::Utc::now().timestamp_millis()));
    let temp_path_str = temp_path.to_string_lossy().to_string();
    
    ftp_manager.download_file_with_progress(&path, &temp_path_str, |_| {})
        .await
        .map_err(|e| format!("Failed to download for preview: {}", e))?;
    
    // Read content
    let content = tokio::fs::read_to_string(&temp_path)
        .await
        .map_err(|e| format!("Failed to read preview content: {}", e))?;
    
    // Clean up temp file
    let _ = tokio::fs::remove_file(&temp_path).await;
    
    Ok(content)
}

#[tauri::command]
async fn save_local_file(path: String, content: String) -> Result<(), String> {
    // Write content to local file
    tokio::fs::write(&path, content)
        .await
        .map_err(|e| format!("Failed to save file: {}", e))?;
    
    Ok(())
}

#[tauri::command]
async fn save_remote_file(state: State<'_, AppState>, path: String, content: String) -> Result<(), String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    
    // Write content to temp file first
    let temp_path = std::env::temp_dir().join(format!("aeroftp_upload_{}", chrono::Utc::now().timestamp_millis()));
    let temp_path_str = temp_path.to_string_lossy().to_string();
    
    tokio::fs::write(&temp_path, &content)
        .await
        .map_err(|e| format!("Failed to write temp file: {}", e))?;
    
    // Upload to remote server
    ftp_manager.upload_file_with_progress(&temp_path_str, &path, content.len() as u64, |_| {})
        .await
        .map_err(|e| format!("Failed to upload file: {}", e))?;
    
    // Clean up temp file
    let _ = tokio::fs::remove_file(&temp_path).await;
    
    Ok(())
}

#[tauri::command]
fn toggle_menu_bar(app: AppHandle, window: tauri::Window, visible: bool) {
    if visible {
        if let Some(menu) = app.menu() {
            let _ = window.set_menu(menu);
        }
    } else {
        let _ = window.remove_menu();
    }
}

// ============ Sync Commands ============

use sync::{
    CompareOptions, FileComparison, FileInfo, SyncIndex,
    build_comparison_results_with_index, should_exclude,
    load_sync_index, save_sync_index,
};
use cloud_config::{CloudConfig, CloudSyncStatus, ConflictStrategy};
use std::collections::HashMap;

#[tauri::command]
async fn compare_directories(
    app: AppHandle,
    state: State<'_, AppState>,
    local_path: String,
    remote_path: String,
    options: Option<CompareOptions>,
) -> Result<Vec<FileComparison>, String> {
    let options = options.unwrap_or_default();

    info!("Comparing directories: local={}, remote={}", local_path, remote_path);

    // Emit scan phase: local
    let _ = app.emit("sync_scan_progress", serde_json::json!({
        "phase": "local",
        "files_found": 0,
    }));

    // Get local files
    let local_files = get_local_files_recursive(&local_path, &local_path, &options.exclude_patterns)
        .await
        .map_err(|e| format!("Failed to scan local directory: {}", e))?;

    // Emit scan phase: remote (with local count)
    let _ = app.emit("sync_scan_progress", serde_json::json!({
        "phase": "remote",
        "files_found": local_files.len(),
    }));

    // Get remote files with progress
    let mut ftp_manager = state.ftp_manager.lock().await;
    let remote_files = get_remote_files_recursive_with_progress(&app, &mut ftp_manager, &remote_path, &remote_path, &options.exclude_patterns, local_files.len())
        .await
        .map_err(|e| format!("Failed to scan remote directory: {}", e))?;

    // Emit scan phase: comparing
    let _ = app.emit("sync_scan_progress", serde_json::json!({
        "phase": "comparing",
        "files_found": local_files.len() + remote_files.len(),
    }));

    // Load sync index if available for conflict detection
    let index = load_sync_index(&local_path, &remote_path).ok().flatten();
    let results = build_comparison_results_with_index(local_files, remote_files, &options, index.as_ref());

    info!("Comparison complete: {} differences found (index: {})", results.len(), if index.is_some() { "used" } else { "none" });

    Ok(results)
}

/// Scan local directory iteratively and build file info map
pub async fn get_local_files_recursive(
    base_path: &str,
    _current_path: &str,
    exclude_patterns: &[String],
) -> Result<HashMap<String, FileInfo>, String> {
    let mut files = HashMap::new();
    let base = PathBuf::from(base_path);
    
    if !base.exists() {
        return Ok(files);
    }
    
    // Use a stack for iterative traversal instead of recursion
    let mut dirs_to_process = vec![base.clone()];
    
    while let Some(current_dir) = dirs_to_process.pop() {
        let mut entries = match tokio::fs::read_dir(&current_dir).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            
            // Calculate relative path
            let relative_path = path
                .strip_prefix(&base)
                .map(|p| p.to_string_lossy().to_string().replace('\\', "/"))
                .unwrap_or_else(|_| name.clone());
            
            // Skip excluded paths
            if should_exclude(&relative_path, exclude_patterns) {
                continue;
            }
            
            let metadata = entry.metadata().await.ok();
            let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            
            let modified = metadata.as_ref().and_then(|m| {
                m.modified().ok().map(|t| {
                    let datetime: chrono::DateTime<chrono::Utc> = t.into();
                    datetime
                })
            });
            
            let size = if is_dir {
                0
            } else {
                metadata.as_ref().map(|m| m.len()).unwrap_or(0)
            };
            
            let file_info = FileInfo {
                name: name.clone(),
                path: path.to_string_lossy().to_string(),
                size,
                modified,
                is_dir,
                checksum: None,
            };
            
            files.insert(relative_path, file_info);
            
            // Add subdirectories to process
            if is_dir {
                dirs_to_process.push(path);
            }
        }
    }
    
    Ok(files)
}

/// Scan remote directory iteratively and build file info map
async fn get_remote_files_recursive(
    ftp_manager: &mut ftp::FtpManager,
    base_path: &str,
    _current_path: &str,
    exclude_patterns: &[String],
) -> Result<HashMap<String, FileInfo>, String> {
    let mut files = HashMap::new();
    
    // Use a stack for iterative traversal
    let mut dirs_to_process = vec![base_path.to_string()];
    
    while let Some(current_dir) = dirs_to_process.pop() {
        // Change to the directory
        if let Err(e) = ftp_manager.change_dir(&current_dir).await {
            info!("Warning: Could not change to directory {}: {}", current_dir, e);
            continue;
        }
        
        // List files
        let entries = match ftp_manager.list_files().await {
            Ok(e) => e,
            Err(e) => {
                info!("Warning: Could not list files in {}: {}", current_dir, e);
                continue;
            }
        };
        
        for entry in entries {
            // Skip . and ..
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            
            // Calculate relative path
            let relative_path = if current_dir == base_path {
                entry.name.clone()
            } else {
                let rel_dir = current_dir.strip_prefix(base_path).unwrap_or(&current_dir);
                let rel_dir = rel_dir.trim_start_matches('/');
                if rel_dir.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", rel_dir, entry.name)
                }
            };
            
            // Skip excluded paths
            if should_exclude(&relative_path, exclude_patterns) {
                continue;
            }
            
            let file_info = FileInfo {
                name: entry.name.clone(),
                path: format!("{}/{}", current_dir, entry.name),
                size: entry.size.unwrap_or(0),
                modified: entry.modified.and_then(|s| {
                    chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M")
                        .ok()
                        .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc))
                }),
                is_dir: entry.is_dir,
                checksum: None,
            };
            
            files.insert(relative_path, file_info);
            
            // Add subdirectories to process
            if entry.is_dir {
                let sub_path = format!("{}/{}", current_dir, entry.name);
                dirs_to_process.push(sub_path);
            }
        }
    }
    
    // Return to base path
    let _ = ftp_manager.change_dir(base_path).await;

    Ok(files)
}

/// Scan remote directory with progress events
async fn get_remote_files_recursive_with_progress(
    app: &AppHandle,
    ftp_manager: &mut ftp::FtpManager,
    base_path: &str,
    _current_path: &str,
    exclude_patterns: &[String],
    local_count: usize,
) -> Result<HashMap<String, FileInfo>, String> {
    let mut files = HashMap::new();
    let mut dirs_to_process = vec![base_path.to_string()];

    while let Some(current_dir) = dirs_to_process.pop() {
        if let Err(e) = ftp_manager.change_dir(&current_dir).await {
            info!("Warning: Could not change to directory {}: {}", current_dir, e);
            continue;
        }

        let entries = match ftp_manager.list_files().await {
            Ok(e) => e,
            Err(e) => {
                info!("Warning: Could not list files in {}: {}", current_dir, e);
                continue;
            }
        };

        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }

            let relative_path = if current_dir == base_path {
                entry.name.clone()
            } else {
                let rel_dir = current_dir.strip_prefix(base_path).unwrap_or(&current_dir);
                let rel_dir = rel_dir.trim_start_matches('/');
                if rel_dir.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", rel_dir, entry.name)
                }
            };

            if should_exclude(&relative_path, exclude_patterns) {
                continue;
            }

            let file_info = FileInfo {
                name: entry.name.clone(),
                path: format!("{}/{}", current_dir, entry.name),
                size: entry.size.unwrap_or(0),
                modified: entry.modified.and_then(|s| {
                    chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M")
                        .ok()
                        .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc))
                }),
                is_dir: entry.is_dir,
                checksum: None,
            };

            files.insert(relative_path, file_info);

            if entry.is_dir {
                dirs_to_process.push(format!("{}/{}", current_dir, entry.name));
            }
        }

        // Emit progress after each directory listing
        let _ = app.emit("sync_scan_progress", serde_json::json!({
            "phase": "remote",
            "files_found": local_count + files.len(),
        }));
    }

    let _ = ftp_manager.change_dir(base_path).await;
    Ok(files)
}

#[tauri::command]
fn get_compare_options_default() -> CompareOptions {
    CompareOptions::default()
}

#[tauri::command]
fn load_sync_index_cmd(local_path: String, remote_path: String) -> Result<Option<SyncIndex>, String> {
    load_sync_index(&local_path, &remote_path)
}

#[tauri::command]
fn save_sync_index_cmd(index: SyncIndex) -> Result<(), String> {
    save_sync_index(&index)
}

// ============ AI Commands ============

#[tauri::command]
async fn ai_chat(request: ai::AIRequest) -> Result<ai::AIResponse, String> {
    ai::call_ai(request)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn ai_test_provider(
    provider_type: ai::AIProviderType,
    base_url: String,
    api_key: Option<String>,
) -> Result<bool, String> {
    ai::test_provider(provider_type, base_url, api_key)
        .await
        .map_err(|e| e.to_string())
}

// Tool execution request
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolRequest {
    tool_name: String,
    args: serde_json::Value,
}

// Allowed AI tool names (whitelist)
const ALLOWED_AI_TOOLS: &[&str] = &[
    "list_files", "read_file", "create_folder", "delete_file",
    "rename_file", "download_file", "upload_file", "chmod",
];

/// Validate and sanitize a path argument from AI tool calls.
/// Rejects null bytes, path traversal sequences, and excessively long paths.
fn validate_tool_path(path: &str, param_name: &str) -> Result<(), String> {
    if path.len() > 4096 {
        return Err(format!("{}: path exceeds 4096 characters", param_name));
    }
    if path.contains('\0') {
        return Err(format!("{}: path contains null bytes", param_name));
    }
    // Reject path traversal: literal ".." components
    for component in path.split('/') {
        if component == ".." {
            return Err(format!("{}: path traversal ('..') is not allowed", param_name));
        }
    }
    // Also check backslash-separated (Windows paths)
    for component in path.split('\\') {
        if component == ".." {
            return Err(format!("{}: path traversal ('..') is not allowed", param_name));
        }
    }
    Ok(())
}

/// Validate a chmod mode string (must be octal digits, 3-4 chars).
fn validate_chmod_mode(mode: &str) -> Result<(), String> {
    if mode.len() < 3 || mode.len() > 4 {
        return Err("mode must be 3-4 octal digits (e.g. '755')".to_string());
    }
    if !mode.chars().all(|c| c.is_ascii_digit() && c <= '7') {
        return Err("mode must contain only octal digits (0-7)".to_string());
    }
    Ok(())
}

// Execute AI tool - routes to existing FTP commands
#[tauri::command]
async fn ai_execute_tool(
    state: State<'_, AppState>,
    app: AppHandle,
    request: ToolRequest,
) -> Result<serde_json::Value, String> {
    // Validate tool name against whitelist
    if !ALLOWED_AI_TOOLS.contains(&request.tool_name.as_str()) {
        return Err(format!("Unknown or disallowed tool: {}", request.tool_name));
    }

    let args = request.args;
    
    match request.tool_name.as_str() {
        "list_files" => {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("remote");
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("/");
            validate_tool_path(path, "path")?;
            
            if location == "local" {
                let files = get_local_files(path.to_string(), Some(true))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "success": true,
                    "count": files.len(),
                    "files": files.iter().take(20).map(|f| {
                        serde_json::json!({
                            "name": f.name,
                            "is_dir": f.is_dir,
                            "size": f.size
                        })
                    }).collect::<Vec<_>>()
                }))
            } else {
                let mut manager = state.ftp_manager.lock().await;
                let files = manager.list_files().await.map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "success": true,
                    "count": files.len(),
                    "files": files.iter().take(20).map(|f| {
                        serde_json::json!({
                            "name": f.name,
                            "is_dir": f.is_dir,
                            "size": f.size
                        })
                    }).collect::<Vec<_>>()
                }))
            }
        },
        
        "read_file" => {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("remote");
            let path = args.get("path").and_then(|v| v.as_str()).ok_or("path required")?;
            validate_tool_path(path, "path")?;

            if location == "local" {
                let content = read_local_file(path.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "success": true,
                    "content": content.chars().take(5000).collect::<String>(),
                    "truncated": content.len() > 5000
                }))
            } else {
                let content = preview_remote_file(state.clone(), path.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "success": true,
                    "content": content.chars().take(5000).collect::<String>(),
                    "truncated": content.len() > 5000
                }))
            }
        },
        
        "create_folder" => {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("remote");
            let path = args.get("path").and_then(|v| v.as_str()).ok_or("path required")?;
            validate_tool_path(path, "path")?;

            if location == "local" {
                create_local_folder(path.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                create_remote_folder(state.clone(), path.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok(serde_json::json!({ "success": true, "message": format!("Created folder: {}", path) }))
        },
        
        "delete_file" => {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("remote");
            let path = args.get("path").and_then(|v| v.as_str()).ok_or("path required")?;
            validate_tool_path(path, "path")?;

            if location == "local" {
                delete_local_file(app.clone(), path.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                // Assume file, not directory for simple delete
                delete_remote_file(app.clone(), state.clone(), path.to_string(), false)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok(serde_json::json!({ "success": true, "message": format!("Deleted: {}", path) }))
        },
        
        "rename_file" => {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("remote");
            let old_path = args.get("old_path").and_then(|v| v.as_str()).ok_or("old_path required")?;
            let new_path = args.get("new_path").and_then(|v| v.as_str()).ok_or("new_path required")?;
            validate_tool_path(old_path, "old_path")?;
            validate_tool_path(new_path, "new_path")?;

            if location == "local" {
                rename_local_file(old_path.to_string(), new_path.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                rename_remote_file(state.clone(), old_path.to_string(), new_path.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok(serde_json::json!({ "success": true, "message": format!("Renamed {} to {}", old_path, new_path) }))
        },
        
        "download_file" => {
            let remote_path = args.get("remote_path").and_then(|v| v.as_str()).ok_or("remote_path required")?;
            let local_path = args.get("local_path").and_then(|v| v.as_str()).ok_or("local_path required")?;
            validate_tool_path(remote_path, "remote_path")?;
            validate_tool_path(local_path, "local_path")?;

            download_file(app, state.clone(), DownloadParams {
                remote_path: remote_path.to_string(),
                local_path: local_path.to_string(),
            }).await.map_err(|e| e.to_string())?;
            
            Ok(serde_json::json!({ "success": true, "message": format!("Downloaded {} to {}", remote_path, local_path) }))
        },
        
        "upload_file" => {
            let local_path = args.get("local_path").and_then(|v| v.as_str()).ok_or("local_path required")?;
            let remote_path = args.get("remote_path").and_then(|v| v.as_str()).ok_or("remote_path required")?;
            validate_tool_path(local_path, "local_path")?;
            validate_tool_path(remote_path, "remote_path")?;

            upload_file(app, state.clone(), UploadParams {
                local_path: local_path.to_string(),
                remote_path: remote_path.to_string(),
            }).await.map_err(|e| e.to_string())?;
            
            Ok(serde_json::json!({ "success": true, "message": format!("Uploaded {} to {}", local_path, remote_path) }))
        },
        
        "chmod" => {
            let path = args.get("path").and_then(|v| v.as_str()).ok_or("path required")?;
            let mode = args.get("mode").and_then(|v| v.as_str()).ok_or("mode required")?;
            validate_tool_path(path, "path")?;
            validate_chmod_mode(mode)?;

            chmod_remote_file(state.clone(), path.to_string(), mode.to_string())
                .await
                .map_err(|e| e.to_string())?;
            
            Ok(serde_json::json!({ "success": true, "message": format!("Changed permissions of {} to {}", path, mode) }))
        },
        
        _ => unreachable!() // tool_name already validated against ALLOWED_AI_TOOLS
    }
}

// ============ AeroCloud Commands ============

#[tauri::command]
fn get_cloud_config() -> CloudConfig {
    cloud_config::load_cloud_config()
}

#[tauri::command]
fn save_cloud_config_cmd(config: CloudConfig) -> Result<(), String> {
    cloud_config::save_cloud_config(&config)
}

#[tauri::command]
async fn setup_aerocloud(
    cloud_name: String,
    local_folder: String,
    remote_folder: String,
    server_profile: String,
    sync_on_change: bool,
    sync_interval_secs: u64,
) -> Result<CloudConfig, String> {
    let mut config = CloudConfig::default();
    config.enabled = true;
    config.cloud_name = cloud_name;
    config.local_folder = std::path::PathBuf::from(&local_folder);
    config.remote_folder = remote_folder.clone();
    config.server_profile = server_profile;
    config.sync_on_change = sync_on_change;
    config.sync_interval_secs = sync_interval_secs;
    
    // Validate configuration
    cloud_config::validate_config(&config)?;
    
    // Ensure local cloud folder exists
    cloud_config::ensure_cloud_folder(&config)?;
    
    // Save configuration
    cloud_config::save_cloud_config(&config)?;
    
    info!("AeroCloud setup complete: local={}, remote={}", local_folder, remote_folder);
    
    Ok(config)
}

#[tauri::command]
fn get_cloud_status() -> CloudSyncStatus {
    let config = cloud_config::load_cloud_config();
    
    if !config.enabled {
        return CloudSyncStatus::NotConfigured;
    }
    
    CloudSyncStatus::Idle {
        last_sync: config.last_sync,
        next_sync: None, // Will be calculated by sync service
    }
}

#[tauri::command]
fn enable_aerocloud(enabled: bool) -> Result<CloudConfig, String> {
    let mut config = cloud_config::load_cloud_config();
    
    if enabled {
        // Validate before enabling
        cloud_config::validate_config(&config)?;
        cloud_config::ensure_cloud_folder(&config)?;
    }
    
    config.enabled = enabled;
    cloud_config::save_cloud_config(&config)?;
    
    info!("AeroCloud {}", if enabled { "enabled" } else { "disabled" });
    
    Ok(config)
}

/// Generate a shareable link for a file in AeroCloud
/// Returns the public URL if public_url_base is configured
#[tauri::command]
fn generate_share_link(local_path: String) -> Result<String, String> {
    let config = cloud_config::load_cloud_config();
    
    if !config.enabled {
        return Err("AeroCloud is not enabled".to_string());
    }
    
    let public_base = config.public_url_base.as_ref()
        .ok_or_else(|| "Public URL not configured. Go to AeroCloud Settings to set your public URL base.".to_string())?;
    
    let local_folder = config.local_folder.to_string_lossy();
    let local_path_str = local_path.clone();
    
    // Check if file is within AeroCloud folder
    let local_folder_str: &str = local_folder.as_ref();
    if !local_path_str.starts_with(local_folder_str) {
        return Err("File is not in AeroCloud folder".to_string());
    }

    // Get relative path from AeroCloud folder
    let relative_path = local_path_str
        .strip_prefix(local_folder_str)
        .unwrap_or(&local_path_str)
        .trim_start_matches('/');
    
    // Construct public URL
    let base = public_base.trim_end_matches('/');
    let url = format!("{}/{}", base, relative_path);
    
    info!("Generated share link: {}", url);
    
    Ok(url)
}

/// Generate share link from remote path (when browsing remote files)
#[tauri::command]
fn generate_share_link_remote(remote_path: String) -> Result<String, String> {
    let config = cloud_config::load_cloud_config();
    
    if !config.enabled {
        return Err("AeroCloud is not enabled".to_string());
    }
    
    let public_base = config.public_url_base.as_ref()
        .ok_or_else(|| "Public URL not configured. Go to AeroCloud Settings to set your public URL base.".to_string())?;
    
    // Check if path is within AeroCloud remote folder
    let remote_folder = config.remote_folder.trim_end_matches('/');
    if !remote_path.starts_with(remote_folder) {
        return Err("File is not in AeroCloud remote folder".to_string());
    }
    
    // Get relative path from remote folder
    let relative_path = remote_path
        .strip_prefix(remote_folder)
        .unwrap_or(&remote_path)
        .trim_start_matches('/');
    
    // Construct public URL
    let base = public_base.trim_end_matches('/');
    let url = format!("{}/{}", base, relative_path);
    
    info!("Generated share link (remote): {}", url);
    
    Ok(url)
}

#[tauri::command]
fn get_default_cloud_folder() -> String {
    let default_config = CloudConfig::default();
    default_config.local_folder.to_string_lossy().to_string()
}

#[tauri::command]
fn update_conflict_strategy(strategy: ConflictStrategy) -> Result<(), String> {
    let mut config = cloud_config::load_cloud_config();
    config.conflict_strategy = strategy;
    cloud_config::save_cloud_config(&config)
}

#[tauri::command]
async fn trigger_cloud_sync(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let config = cloud_config::load_cloud_config();
    
    info!("AeroCloud sync triggered");
    info!("Config - enabled: {}, local: {:?}, remote: {}", 
        config.enabled, config.local_folder, config.remote_folder);
    
    if !config.enabled {
        return Err("AeroCloud is not configured. Please set it up first.".to_string());
    }
    
    // Get FTP manager and perform sync
    let mut ftp_manager = state.ftp_manager.lock().await;
    
    if !ftp_manager.is_connected() {
        return Err("Not connected to FTP server. Please connect first.".to_string());
    }
    
    info!("FTP connected, starting sync...");
    
    // First, ensure remote folder exists and navigate to it
    if let Err(_e) = ftp_manager.change_dir(&config.remote_folder).await {
        info!("Remote folder {} doesn't exist, creating it...", config.remote_folder);
        // Try to create the folder
        if let Err(e) = ftp_manager.mkdir(&config.remote_folder).await {
            warn!("Could not create remote folder: {}", e);
        }
    }
    
    // Create cloud service and run sync
    let cloud_service = cloud_service::CloudService::new();
    cloud_service.init(config.clone()).await;
    
    match cloud_service.perform_full_sync(&mut ftp_manager).await {
        Ok(result) => {
            let summary = format!(
                "Sync complete: {} uploaded, {} downloaded, {} conflicts, {} skipped, {} errors",
                result.uploaded, result.downloaded, result.conflicts, result.skipped, result.errors.len()
            );
            info!("{}", summary);
            if !result.errors.is_empty() {
                for err in &result.errors {
                    warn!("Sync error: {}", err);
                }
            }
            Ok(summary)
        }
        Err(e) => {
            error!("Sync failed: {}", e);
            Err(format!("Sync failed: {}", e))
        }
    }
}
// ============ Background Sync & Tray Commands ============

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

// Global flag to control background sync
static BACKGROUND_SYNC_RUNNING: AtomicBool = AtomicBool::new(false);

/// Background sync worker - runs in a separate tokio task
/// Creates its own FTP connection to avoid conflicts with main UI
async fn background_sync_worker(app: AppHandle) {
    info!("Background sync worker started");
    
    // Run first sync immediately, then loop with intervals
    let mut is_first_run = true;
    
    loop {
        // Check if we should stop
        if !BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
            info!("Background sync worker stopping (flag set to false)");
            break;
        }
        
        // Load fresh config each cycle
        let config = cloud_config::load_cloud_config();
        if !config.enabled {
            info!("AeroCloud disabled, stopping background sync");
            BACKGROUND_SYNC_RUNNING.store(false, Ordering::SeqCst);
            let _ = app.emit("cloud-sync-status", serde_json::json!({
                "status": "disabled",
                "message": "AeroCloud is disabled"
            }));
            break;
        }
        
        // On first run, sync immediately. On subsequent runs, wait for interval first.
        if !is_first_run {
            let interval_secs = config.sync_interval_secs.max(30); // Minimum 30 seconds for testing
            info!("Background sync: next sync in {}s", interval_secs);
            
            // Wait for interval (check cancel flag every 5 seconds)
            let mut waited = 0u64;
            while waited < interval_secs {
                if !BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
                    info!("Background sync cancelled during wait");
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                waited += 5;
            }
            
            // Check again after waiting
            if !BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
                break;
            }
        }
        is_first_run = false;
        
        // Perform sync with dedicated FTP connection
        info!("Background sync: starting sync cycle");
        
        // Emit syncing status
        let _ = app.emit("cloud-sync-status", serde_json::json!({
            "status": "syncing",
            "message": "Syncing..."
        }));
        
        match perform_background_sync(&config).await {
            Ok(result) => {
                info!("Background sync completed: {} uploaded, {} downloaded, {} errors",
                    result.uploaded, result.downloaded, result.errors.len());
                
                // Emit success
                let _ = app.emit("cloud-sync-status", serde_json::json!({
                    "status": "active",
                    "message": format!("Synced: ↑{} ↓{}", result.uploaded, result.downloaded)
                }));
                
                // Emit sync complete event for UI
                let _ = app.emit("cloud_sync_complete", &result);
            }
            Err(e) => {
                warn!("Background sync failed: {}", e);
                let _ = app.emit("cloud-sync-status", serde_json::json!({
                    "status": "error",
                    "message": format!("Sync failed: {}", e)
                }));
                
                // On error, wait a bit before retrying to avoid spamming
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }
    
    info!("Background sync worker exited");
}

/// Perform a sync cycle with a dedicated FTP connection
/// This avoids conflicts with the main UI FTP connection
async fn perform_background_sync(config: &cloud_config::CloudConfig) -> Result<cloud_service::SyncOperationResult, String> {
    // Load saved server credentials
    let server_profile = &config.server_profile;
    
    // Try to get server credentials from saved servers (localStorage sync)
    // For now, we'll use a simple approach - the sync requires active main connection
    // In the future, we could store encrypted credentials
    
    // Create dedicated FTP manager for background sync
    let mut ftp_manager = ftp::FtpManager::new();
    
    // We need server credentials - check if we have them saved
    // For MVP, background sync only works if the server is already connected in main UI
    // and we use the same credentials from config file
    
    // Load credentials from secure store (keyring or vault)
    info!("Background sync: loading credentials for profile '{}'", server_profile);

    let store = get_credential_store()
        .map_err(|e| format!("Background sync: no credential store available: {}", e))?;

    let creds_json = store.get(&format!("server_{}", server_profile))
        .map_err(|e| format!("No saved credentials for profile '{}': {}", server_profile, e))?;

    #[derive(serde::Deserialize)]
    struct SavedCredentials {
        server: String,
        username: String,
        password: String,
    }

    let creds: SavedCredentials = serde_json::from_str(&creds_json)
        .map_err(|e| format!("Failed to parse credentials for '{}': {}", server_profile, e))?;

    // Connect to server
    ftp_manager.connect(&creds.server)
        .await
        .map_err(|e| format!("Failed to connect for background sync: {}", e))?;

    // Login with credentials
    ftp_manager.login(&creds.username, &creds.password)
        .await
        .map_err(|e| format!("Failed to login for background sync: {}", e))?;

    info!("Background sync: connected to {} as {}", creds.server, creds.username);
    
    // Navigate to remote folder
    if ftp_manager.change_dir(&config.remote_folder).await.is_err() {
        // Try to create it
        let _ = ftp_manager.mkdir(&config.remote_folder).await;
        ftp_manager.change_dir(&config.remote_folder).await
            .map_err(|e| format!("Failed to navigate to remote folder: {}", e))?;
    }
    
    // Create cloud service and perform sync
    let cloud_service = cloud_service::CloudService::new();
    cloud_service.init(config.clone()).await;
    
    let result = cloud_service.perform_full_sync(&mut ftp_manager).await?;
    
    // Disconnect
    let _ = ftp_manager.disconnect().await;
    
    Ok(result)
}

#[tauri::command]
async fn start_background_sync(
    app: AppHandle,
    _state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    if BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
        return Ok("Background sync already running".to_string());
    }

    let config = cloud_config::load_cloud_config();
    if !config.enabled {
        return Err("AeroCloud not configured".to_string());
    }

    // Set flag before spawning
    BACKGROUND_SYNC_RUNNING.store(true, Ordering::SeqCst);
    
    // Clone app handle for the spawned task
    let app_clone = app.clone();
    
    // Spawn background worker
    tokio::spawn(async move {
        background_sync_worker(app_clone).await;
    });
    
    // Emit status
    let _ = app.emit("cloud-sync-status", serde_json::json!({
        "status": "active",
        "message": "Background sync started"
    }));

    info!("Background sync started with interval: {}s", config.sync_interval_secs);
    
    Ok(format!("Background sync started (interval: {}s)", config.sync_interval_secs))
}

#[tauri::command]
async fn stop_background_sync(app: AppHandle) -> Result<String, String> {
    if !BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
        return Ok("Background sync not running".to_string());
    }

    BACKGROUND_SYNC_RUNNING.store(false, Ordering::SeqCst);
    
    // Emit status
    let _ = app.emit("cloud-sync-status", serde_json::json!({
        "status": "idle",
        "message": "Background sync stopped"
    }));

    info!("Background sync stopped");
    
    Ok("Background sync stopped".to_string())
}

#[tauri::command]
fn is_background_sync_running() -> bool {
    BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst)
}

#[tauri::command]
async fn set_tray_status(app: AppHandle, status: String, tooltip: Option<String>) -> Result<(), String> {
    let _ = app.emit("tray-status-update", serde_json::json!({
        "status": status,
        "tooltip": tooltip.unwrap_or_else(|| "AeroCloud".to_string())
    }));
    
    info!("Tray status updated: {}", status);
    Ok(())
}

/// Save server credentials for background sync use (now uses secure credential store)
#[tauri::command]
async fn save_server_credentials(
    profile_name: String,
    server: String,
    username: String,
    password: String,
) -> Result<(), String> {
    let store = get_credential_store()?;

    let value = serde_json::json!({
        "server": server,
        "username": username,
        "password": password,
    });

    store.store_and_track(
        &format!("server_{}", profile_name),
        &value.to_string(),
    ).map_err(|e| format!("Failed to save credentials: {}", e))?;

    info!("Saved credentials for profile: {}", profile_name);
    Ok(())
}

// ============ Secure Credential Store Commands ============

#[derive(Serialize)]
struct CredentialStatus {
    backend: String,
    accounts_count: u32,
    keyring_available: bool,
    vault_exists: bool,
}

#[tauri::command]
async fn check_keyring_available() -> Result<bool, String> {
    Ok(credential_store::CredentialStore::is_keyring_available())
}

#[tauri::command]
async fn get_credential_status() -> Result<CredentialStatus, String> {
    let keyring_available = credential_store::CredentialStore::is_keyring_available();
    let vault_exists = credential_store::CredentialStore::vault_exists();

    let (backend, accounts_count) = if keyring_available {
        match credential_store::CredentialStore::with_keyring() {
            Some(store) => {
                let count = store.list_accounts().unwrap_or_default().len() as u32;
                ("keyring".to_string(), count)
            }
            None => ("none".to_string(), 0),
        }
    } else if vault_exists {
        ("vault_locked".to_string(), 0)
    } else {
        ("none".to_string(), 0)
    };

    Ok(CredentialStatus {
        backend,
        accounts_count,
        keyring_available,
        vault_exists,
    })
}

#[tauri::command]
async fn store_credential(account: String, password: String) -> Result<(), String> {
    // Try credential store (probe-based)
    if let Ok(store) = get_credential_store() {
        return store.store_and_track(&account, &password)
            .map_err(|e| format!("Failed to store credential: {}", e));
    }
    // Fallback: try direct keyring access without probe
    let entry = keyring::Entry::new("aeroftp", &account)
        .map_err(|e| format!("Keyring unavailable: {}", e))?;
    entry.set_password(&password)
        .map_err(|e| format!("Failed to store credential: {}", e))
}

#[tauri::command]
async fn get_credential(account: String) -> Result<String, String> {
    // Try credential store (probe-based)
    if let Ok(store) = get_credential_store() {
        return store.get(&account)
            .map_err(|e| format!("Failed to get credential: {}", e));
    }
    // Fallback: try direct keyring access without probe
    // On Linux, gnome-keyring may reject probe but serve real credentials
    let entry = keyring::Entry::new("aeroftp", &account)
        .map_err(|e| format!("Keyring unavailable: {}", e))?;
    entry.get_password()
        .map_err(|e| format!("Failed to get credential: {}", e))
}

#[tauri::command]
async fn delete_credential(account: String) -> Result<(), String> {
    let store = get_credential_store()?;
    store.delete_and_track(&account)
        .map_err(|e| format!("Failed to delete credential: {}", e))
}

#[tauri::command]
async fn setup_master_password(password: String) -> Result<(), String> {
    credential_store::CredentialStore::setup_vault(&password)
        .map_err(|e| format!("Failed to setup vault: {}", e))?;
    info!("Master password vault initialized");
    Ok(())
}

#[tauri::command]
async fn unlock_vault(password: String) -> Result<(), String> {
    // Verify the password works
    credential_store::CredentialStore::with_vault(&password)
        .map_err(|e| format!("Failed to unlock vault: {}", e))?;
    info!("Vault unlocked successfully");
    Ok(())
}

#[tauri::command]
async fn migrate_plaintext_credentials() -> Result<credential_store::MigrationResult, String> {
    let store = get_credential_store()?;

    // Migrate server credentials
    let mut result = credential_store::migrate_server_credentials(&store)
        .map_err(|e| format!("Migration failed: {}", e))?;

    // Migrate OAuth tokens
    match credential_store::migrate_oauth_tokens(&store) {
        Ok(count) => result.migrated_count += count,
        Err(e) => result.errors.push(format!("OAuth migration: {}", e)),
    }

    // Harden all config directory permissions
    let _ = credential_store::harden_config_directory();

    info!("Migration complete: {} credentials migrated", result.migrated_count);
    Ok(result)
}

/// Helper to get an active credential store (keyring preferred, vault fallback)
fn get_credential_store() -> Result<credential_store::CredentialStore, String> {
    // Try OS keyring first
    if let Some(store) = credential_store::CredentialStore::with_keyring() {
        return Ok(store);
    }
    // Vault fallback - it needs to be unlocked already
    // For now, return error if neither is available
    Err("No credential store available. OS keyring not found and vault not configured.".to_string())
}

// ============ Profile Export/Import ============

#[tauri::command]
async fn export_server_profiles(
    servers_json: String,
    password: String,
    include_credentials: bool,
    file_path: String,
) -> Result<profile_export::ExportMetadata, String> {
    let mut servers: Vec<profile_export::ServerProfileExport> = serde_json::from_str(&servers_json)
        .map_err(|e| format!("Invalid server data: {}", e))?;

    // Fetch credentials from secure store if requested
    if include_credentials {
        if let Ok(store) = get_credential_store() {
            for server in &mut servers {
                if let Ok(cred) = store.get(&format!("server_{}", server.id)) {
                    server.credential = Some(cred);
                }
            }
        }
    }

    profile_export::export_profiles(servers, &password, std::path::Path::new(&file_path))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn import_server_profiles(
    file_path: String,
    password: String,
) -> Result<serde_json::Value, String> {
    let (servers, metadata) = profile_export::import_profiles(std::path::Path::new(&file_path), &password)
        .map_err(|e| e.to_string())?;

    // Store credentials in secure store and strip from returned data
    if let Ok(store) = get_credential_store() {
        for server in &servers {
            if let Some(ref cred) = server.credential {
                let _ = store.store_and_track(&format!("server_{}", server.id), cred);
            }
        }
    }

    // Return servers without credentials + metadata
    let result = serde_json::json!({
        "servers": servers,
        "metadata": metadata,
    });
    Ok(result)
}

#[tauri::command]
async fn read_export_metadata(file_path: String) -> Result<profile_export::ExportMetadata, String> {
    profile_export::read_metadata(std::path::Path::new(&file_path))
        .map_err(|e| e.to_string())
}

// ============ App Entry Point ============

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri::menu::{Menu, MenuItem, Submenu, PredefinedMenuItem};

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_log::Builder::default()
            .level(log::LevelFilter::Info)
            .build())
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // When a second instance is launched, show and focus the existing window
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            use tauri::tray::{TrayIconBuilder, MouseButton, MouseButtonState};
            
            // Create menu items
            let quit = MenuItem::with_id(app, "quit", "Quit AeroFTP", true, Some("CmdOrCtrl+Q"))?;
            let about = MenuItem::with_id(app, "about", "About AeroFTP", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "Settings...", true, Some("CmdOrCtrl+,"))?;
            let refresh = MenuItem::with_id(app, "refresh", "Refresh", true, Some("CmdOrCtrl+R"))?;
            let shortcuts = MenuItem::with_id(app, "shortcuts", "Keyboard Shortcuts", true, Some("F1"))?;
            let support = MenuItem::with_id(app, "support", "Support Development ❤️", true, None::<&str>)?;
            
            // File menu
            let file_menu = Submenu::with_items(
                app,
                "File",
                true,
                &[
                    &MenuItem::with_id(app, "new_folder", "New Folder", true, Some("CmdOrCtrl+N"))?,
                    &PredefinedMenuItem::separator(app)?,
                    &settings,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "toggle_debug_mode", "Debug Mode", true, Some("CmdOrCtrl+Shift+F12"))?,
                    &MenuItem::with_id(app, "show_dependencies", "Dependencies...", true, None::<&str>)?,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
                ],
            )?;
            
            // Edit menu
            let edit_menu = Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &MenuItem::with_id(app, "rename", "Rename", true, Some("F2"))?,
                    &MenuItem::with_id(app, "delete", "Delete", true, Some("Delete"))?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ],
            )?;
            
            // View menu
            let devtools_submenu = Submenu::with_items(
                app,
                "DevTools",
                true,
                &[
                    &MenuItem::with_id(app, "toggle_devtools", "Toggle DevTools", true, Some("CmdOrCtrl+Shift+D"))?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "toggle_editor", "Toggle Editor", true, Some("CmdOrCtrl+1"))?,
                    &MenuItem::with_id(app, "toggle_terminal", "Toggle Terminal", true, Some("CmdOrCtrl+2"))?,
                    &MenuItem::with_id(app, "toggle_agent", "Toggle Agent", true, Some("CmdOrCtrl+3"))?,
                ],
            )?;
            
            let view_menu = Submenu::with_items(
                app,
                "View",
                true,
                &[
                    &refresh,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "toggle_theme", "Toggle Theme", true, Some("CmdOrCtrl+T"))?,
                    &PredefinedMenuItem::separator(app)?,
                    &devtools_submenu,
                ],
            )?;
            
            // Help menu
            let help_menu = Submenu::with_items(
                app,
                "Help",
                true,
                &[
                    &shortcuts,
                    &PredefinedMenuItem::separator(app)?,
                    &support,
                    &PredefinedMenuItem::separator(app)?,
                    &about,
                ],
            )?;
            
            let menu = Menu::with_items(app, &[&file_menu, &edit_menu, &view_menu, &help_menu])?;
            app.set_menu(menu)?;
            
            // ============ System Tray Icon ============
            // Create tray menu
            let tray_sync_now = MenuItem::with_id(app, "tray_sync_now", "Sync Now", true, None::<&str>)?;
            let tray_pause = MenuItem::with_id(app, "tray_pause", "Pause Sync", true, None::<&str>)?;
            let tray_open_folder = MenuItem::with_id(app, "tray_open_folder", "Open Cloud Folder", true, None::<&str>)?;
            let tray_check_update = MenuItem::with_id(app, "tray_check_update", "Check for Updates", true, None::<&str>)?;
            let tray_separator = PredefinedMenuItem::separator(app)?;
            let tray_show = MenuItem::with_id(app, "tray_show", "Show AeroFTP", true, None::<&str>)?;
            let tray_quit = MenuItem::with_id(app, "tray_quit", "Quit", true, None::<&str>)?;
            
            let tray_menu = Menu::with_items(app, &[
                &tray_sync_now,
                &tray_pause,
                &tray_separator,
                &tray_open_folder,
                &tray_check_update,
                &PredefinedMenuItem::separator(app)?,
                &tray_show,
                &tray_quit,
            ])?;
            
            // Build tray icon using the app's default icon
            let icon = app.default_window_icon()
                .cloned()
                .expect("Failed to load tray icon - ensure icon is set in tauri.conf.json");
            
            let _tray = TrayIconBuilder::new()
                .icon(icon)
                .tooltip("AeroCloud - Idle")
                .menu(&tray_menu)
                .on_menu_event(|app, event| {
                    let id = event.id().as_ref();
                    info!("Tray menu event: {}", id);
                    match id {
                        "tray_sync_now" => {
                            let _ = app.emit("menu-event", "cloud_sync_now");
                        }
                        "tray_pause" => {
                            let _ = app.emit("menu-event", "cloud_pause");
                        }
                        "tray_open_folder" => {
                            let _ = app.emit("menu-event", "cloud_open_folder");
                        }
                        "tray_check_update" => {
                            let _ = app.emit("menu-event", "check_update");
                        }
                        "tray_show" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "tray_quit" => {
                            std::process::exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    // Click on tray icon shows the window
                    if let tauri::tray::TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        if let Some(window) = tray.app_handle().get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;
            
            info!("System tray icon initialized");
            
            Ok(())
        })
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            info!("Menu event: {}", id);
            // Emit event to frontend
            let _ = app.emit("menu-event", id);
        })
        .on_window_event(|window, event| {
            // Hide window instead of closing when AeroCloud is enabled
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Check if AeroCloud is enabled (should stay in tray)
                let cloud_config = cloud_config::load_cloud_config();
                if cloud_config.enabled {
                    info!("Window close requested, hiding to tray (AeroCloud enabled)");
                    let _ = window.hide();
                    api.prevent_close();
                } else {
                    info!("Window close requested, AeroCloud not enabled, exiting");
                }
            }
        })
        .manage(AppState::new())
        .manage(provider_commands::ProviderState::new())
        .manage(session_manager::MultiProviderState::new());

    // Add PTY state for terminal support (all platforms)
    let builder = builder.manage(create_pty_state());
    // Add SSH shell state for remote shell sessions
    let builder = builder.manage(create_ssh_shell_state());
    let builder = builder.manage(cryptomator::CryptomatorState::new());

    builder
        .invoke_handler(tauri::generate_handler![
            copy_to_clipboard,
            connect_ftp,
            disconnect_ftp,
            check_connection,
            ftp_noop,
            reconnect_ftp,
            list_files,
            change_directory,
            download_file,
            upload_file,
            download_folder,
            upload_folder,
            cancel_transfer,
            set_speed_limit,
            get_speed_limit,
            is_running_as_snap,
            get_local_files,
            open_in_file_manager,
            delete_remote_file,
            rename_remote_file,
            create_remote_folder,
            chmod_remote_file,
            delete_local_file,
            rename_local_file,
            create_local_folder,
            read_file_base64,
            calculate_checksum,
            compress_files,
            extract_archive,
            compress_7z,
            extract_7z,
            is_7z_encrypted,
            is_zip_encrypted,
            extract_rar,
            is_rar_encrypted,
            compress_tar,
            extract_tar,
            ftp_read_file_base64,
            read_local_file,
            read_local_file_base64,
            preview_remote_file,
            save_local_file,
            save_remote_file,
            toggle_menu_bar,
            compare_directories,
            get_compare_options_default,
            load_sync_index_cmd,
            save_sync_index_cmd,
            // AeroCloud commands
            get_cloud_config,
            save_cloud_config_cmd,
            setup_aerocloud,
            get_cloud_status,
            enable_aerocloud,
            generate_share_link,
            generate_share_link_remote,
            get_default_cloud_folder,
            update_conflict_strategy,
            trigger_cloud_sync,
            // Background sync & tray commands
            start_background_sync,
            stop_background_sync,
            is_background_sync_running,
            set_tray_status,
            save_server_credentials,
            // Secure Credential Store commands
            check_keyring_available,
            get_credential_status,
            store_credential,
            get_credential,
            delete_credential,
            export_server_profiles,
            import_server_profiles,
            read_export_metadata,
            setup_master_password,
            unlock_vault,
            migrate_plaintext_credentials,
            // Debug & dependencies commands
            get_dependencies,
            check_crate_versions,
            get_system_info,
            // Updater commands
            check_update,
            log_update_detection,
            download_update,
            install_appimage_update,
            // AI commands
            ai_chat,
            ai_test_provider,
            ai_execute_tool,
            ai_tools::execute_ai_tool,
            // Archive browsing & selective extraction
            archive_browse::list_zip,
            archive_browse::list_7z,
            archive_browse::list_tar,
            archive_browse::list_rar,
            archive_browse::extract_zip_entry,
            archive_browse::extract_7z_entry,
            archive_browse::extract_tar_entry,
            archive_browse::extract_rar_entry,
            // AeroVault encrypted folders
            aerovault::vault_create,
            aerovault::vault_list,
            aerovault::vault_get_meta,
            aerovault::vault_add_files,
            aerovault::vault_remove_file,
            aerovault::vault_extract_entry,
            aerovault::vault_change_password,
            // AeroVault v2 - Military-Grade Encryption
            aerovault_v2::vault_v2_create,
            aerovault_v2::vault_v2_open,
            aerovault_v2::is_vault_v2,
            aerovault_v2::vault_v2_peek,
            aerovault_v2::vault_v2_security_info,
            aerovault_v2::vault_v2_add_files,
            aerovault_v2::vault_v2_extract_entry,
            aerovault_v2::vault_v2_extract_all,
            aerovault_v2::vault_v2_change_password,
            aerovault_v2::vault_v2_delete_entry,
            // Cryptomator vault support
            cryptomator::cryptomator_unlock,
            cryptomator::cryptomator_lock,
            cryptomator::cryptomator_list,
            cryptomator::cryptomator_decrypt_file,
            cryptomator::cryptomator_encrypt_file,
            ai_stream::ai_chat_stream,
            // Multi-protocol provider commands
            provider_commands::provider_connect,
            provider_commands::provider_disconnect,
            provider_commands::provider_check_connection,
            provider_commands::provider_list_files,
            provider_commands::provider_change_dir,
            provider_commands::provider_go_up,
            provider_commands::provider_pwd,
            provider_commands::provider_download_file,
            provider_commands::provider_download_folder,
            provider_commands::provider_upload_file,
            provider_commands::provider_mkdir,
            provider_commands::provider_delete_file,
            provider_commands::provider_delete_dir,
            provider_commands::provider_rename,
            provider_commands::provider_stat,
            provider_commands::provider_keep_alive,
            provider_commands::provider_server_info,
            provider_commands::provider_file_size,
            provider_commands::provider_exists,
            // OAuth2 cloud provider commands
            provider_commands::oauth2_start_auth,
            provider_commands::oauth2_complete_auth,
            provider_commands::oauth2_connect,
            provider_commands::oauth2_full_auth,
            provider_commands::oauth2_has_tokens,
            provider_commands::oauth2_logout,
            provider_commands::provider_create_share_link,
            provider_commands::provider_remove_share_link,
            provider_commands::provider_import_link,
            provider_commands::provider_compare_directories,
            provider_commands::provider_storage_info,
            provider_commands::provider_disk_usage,
            provider_commands::provider_find,
            provider_commands::provider_set_speed_limit,
            provider_commands::provider_get_speed_limit,
            provider_commands::provider_supports_resume,
            provider_commands::provider_resume_download,
            provider_commands::provider_resume_upload,
            // File versions
            provider_commands::provider_supports_versions,
            provider_commands::provider_list_versions,
            provider_commands::provider_download_version,
            provider_commands::provider_restore_version,
            // File locking
            provider_commands::provider_supports_locking,
            provider_commands::provider_lock_file,
            provider_commands::provider_unlock_file,
            // Thumbnails
            provider_commands::provider_supports_thumbnails,
            provider_commands::provider_get_thumbnail,
            // Permissions / Advanced sharing
            provider_commands::provider_supports_permissions,
            provider_commands::provider_list_permissions,
            provider_commands::provider_add_permission,
            provider_commands::provider_remove_permission,
            // Multi-session provider commands
            session_commands::session_connect,
            session_commands::session_disconnect,
            session_commands::session_switch,
            session_commands::session_list,
            session_commands::session_info,
            session_commands::session_list_files,
            session_commands::session_change_dir,
            session_commands::session_mkdir,
            session_commands::session_delete,
            session_commands::session_rename,
            session_commands::session_download,
            session_commands::session_upload,
            session_commands::session_create_share_link,
            spawn_shell,
            pty_write,
            pty_resize,
            pty_close,
            ssh_shell_open,
            ssh_shell_write,
            ssh_shell_resize,
            ssh_shell_close
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}