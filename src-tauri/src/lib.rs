// AeroFTP - Modern FTP Client with Tauri
// Real-time transfer progress with event emission

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tauri::{AppHandle, Emitter, State, Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::Mutex;
use tracing::{info, warn, error};
use semver::Version;
use reqwest::Client as HttpClient;
use secrecy::{ExposeSecret, SecretString};

mod ftp;
mod sync;
mod ai;
mod cloud_config;
mod file_watcher;
mod sync_scheduler;
mod transfer_pool;
mod delta_sync;
mod cloud_service;
mod cloud_provider_factory;
mod providers;
mod provider_commands;
mod session_manager;
mod session_commands;
mod crypto;
mod credential_store;
mod profile_export;
mod keystore_export;
mod pty;
mod ssh_shell;
mod host_key_check;
mod ai_tools;
mod context_intelligence;
mod plugins;
mod plugin_registry;
mod ai_stream;
mod archive_browse;
mod aerovault;
mod aerovault_v2;
mod vault_remote;
mod cryptomator;
mod master_password;
mod windows_acl;
mod filesystem;
mod tray_badge;
mod sync_badge;
mod cyber_tools;
mod totp;
mod chat_history;
mod file_tags;
mod image_edit;
#[cfg(windows)]
mod cloud_filter_badge;

use filesystem::validate_path;
use ftp::{FtpManager, RemoteFile};
use pty::{create_pty_state, spawn_shell, pty_write, pty_resize, pty_close};
use ssh_shell::{create_ssh_shell_state, ssh_shell_open, ssh_shell_write, ssh_shell_resize, ssh_shell_close};
use host_key_check::{sftp_check_host_key, sftp_accept_host_key, sftp_remove_host_key};

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
    cancel_flag: Arc<AtomicBool>,
    speed_limits: SpeedLimits,
}

impl AppState {
    fn new() -> Self {
        Self {
            ftp_manager: Mutex::new(FtpManager::new()),
            cancel_flag: Arc::new(AtomicBool::new(false)),
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
    #[serde(default)]
    file_exists_action: String,
}

#[derive(Serialize, Deserialize)]
pub struct UploadFolderParams {
    local_path: String,
    remote_path: String,
    #[serde(default)]
    file_exists_action: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_files: Option<u64>, // When set, transferred/total are file counts (folder transfer)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>, // Full path for context
}

#[derive(Clone, Serialize)]
pub struct TransferEvent {
    pub event_type: String, // "start", "progress", "complete", "error", "cancelled"
    pub transfer_id: String,
    pub filename: String,
    pub direction: String,
    pub message: Option<String>,
    pub progress: Option<TransferProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>, // Full path for context (file or folder)
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
            // Check if installed via MSI (in Program Files or Program Files (x86))
            if let Ok(exe_path) = std::env::current_exe() {
                let path_str = exe_path.to_string_lossy().to_lowercase();
                // Check against env-provided Program Files paths (more reliable than hardcoded)
                let pf = std::env::var("ProgramFiles").unwrap_or_default().to_lowercase();
                let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_default().to_lowercase();
                if (!pf.is_empty() && path_str.starts_with(&pf))
                    || (!pf86.is_empty() && path_str.starts_with(&pf86))
                    || path_str.contains("program files") {
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
        // On Linux/X11, spawn the clipboard operation in a separate thread.
        // Using .wait() blocks until a clipboard manager reads the content,
        // which can hang indefinitely if no manager is active.
        // We spawn a detached thread to handle this without blocking the UI.
        use arboard::SetExtLinux;
        let text_clone = text.clone();
        std::thread::spawn(move || {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set().wait().text(text_clone);
            }
        });
        // Also set without wait as immediate fallback
        clipboard.set_text(text)
            .map_err(|e| format!("Clipboard write failed: {}", e))?;
    }
    #[cfg(target_os = "windows")]
    {
        // On Windows, spawn clipboard write in a separate thread to avoid
        // potential UI freeze when Credential Manager or Windows Hello is active
        let text_clone = text.clone();
        std::thread::spawn(move || {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_text(text_clone);
            }
        });
        clipboard.set_text(text)
            .map_err(|e| format!("Clipboard write failed: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        clipboard.set_text(text)
            .map_err(|e| format!("Clipboard write failed: {}", e))?;
    }
    Ok(())
}

#[tauri::command]
async fn resolve_hostname(hostname: String, port: u16) -> Result<String, String> {
    let addr = format!("{}:{}", hostname, port);
    let mut addrs = tokio::net::lookup_host(&addr)
        .await
        .map_err(|e| format!("DNS resolution failed: {}", e))?;
    addrs
        .next()
        .map(|a| a.ip().to_string())
        .ok_or_else(|| "No addresses found".to_string())
}

#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Result<UpdateInfo, String> {
    let current_version = app.package_info().version.to_string();
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
    let current = Version::parse(&current_version)
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

        // Only notify when the asset for the installed format is actually available.
        // GitHub releases are published before CI finishes building all artifacts,
        // so the .deb/.rpm/etc. may not exist yet.
        if download_url.is_none() && !extension.is_empty() {
            info!("Update v{} exists but {} asset not yet available, skipping notification",
                  latest_tag, extension);
            return Ok(UpdateInfo {
                has_update: false,
                latest_version: Some(latest_tag.to_string()),
                download_url: None,
                current_version,
                install_format,
            });
        }

        info!("Update available: v{} -> v{} (format: {}, url: {:?})",
              current_version, latest_tag, install_format, download_url);

        return Ok(UpdateInfo {
            has_update: true,
            latest_version: Some(latest_tag.to_string()),
            download_url,
            current_version: current_version.clone(),
            install_format,
        });
    }
    
    info!("No update available. Current: v{}, Latest: v{}", current_version, latest_tag);
    
    Ok(UpdateInfo {
        has_update: false,
        latest_version: Some(latest_tag.to_string()),
        download_url: None,
        current_version,
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

/// Spawn a fully detached relaunch process using setsid.
/// The child runs in its own session so it survives when the parent exits.
#[cfg(unix)]
fn spawn_detached_relaunch(exe_path: &str) {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(format!("sleep 2 && exec '{}'", exe_path))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: setsid() creates a new session, detaching from parent's process group.
    // This is safe because we haven't spawned threads in the child yet.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(_) => info!("Detached relaunch spawned: {}", exe_path),
        Err(e) => tracing::warn!("Failed to spawn relaunch: {}", e),
    }
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

    // Restart via setsid-detached process — survives parent exit
    #[cfg(unix)]
    {
        let exe_path = current_exe.display().to_string();
        spawn_detached_relaunch(&exe_path);
    }
    app.exit(0);

    Ok(())
}

/// Install a .deb package via pkexec with branded Polkit dialog and restart the app.
/// Uses /usr/lib/aeroftp/aeroftp-update-helper (installed by .deb) for branded auth dialog.
/// Falls back to generic `pkexec dpkg -i` if helper is not found.
#[tauri::command]
async fn install_deb_update(app: AppHandle, downloaded_path: String) -> Result<(), String> {
    let downloaded = std::path::Path::new(&downloaded_path);
    if !downloaded.exists() {
        return Err("Downloaded file not found".to_string());
    }

    let helper = std::path::Path::new("/usr/lib/aeroftp/aeroftp-update-helper");
    let status = if helper.exists() {
        info!("Installing .deb update via branded Polkit helper: {}", downloaded_path);
        std::process::Command::new("pkexec")
            .args(["/usr/lib/aeroftp/aeroftp-update-helper", &downloaded_path])
            .status()
    } else {
        info!("Installing .deb update via generic pkexec: {}", downloaded_path);
        std::process::Command::new("pkexec")
            .args(["dpkg", "-i", &downloaded_path])
            .status()
    }
    .map_err(|e| format!("Failed to launch pkexec: {}", e))?;

    if !status.success() {
        return Err(format!("Installation failed (exit code: {:?}). You can install manually with: sudo dpkg -i {}",
            status.code(), downloaded_path));
    }

    info!(".deb installed successfully, restarting...");
    let _ = std::fs::remove_file(downloaded);

    // Restart via setsid-detached process — survives parent exit
    #[cfg(unix)]
    {
        let current_exe = std::env::current_exe()
            .map_err(|e| format!("Cannot find current exe: {}", e))?;
        let exe_path = current_exe.display().to_string();
        spawn_detached_relaunch(&exe_path);
    }
    app.exit(0);

    Ok(())
}

/// Install an .rpm package via pkexec with branded Polkit dialog and restart the app.
/// Same helper/fallback pattern as install_deb_update.
#[tauri::command]
async fn install_rpm_update(app: AppHandle, downloaded_path: String) -> Result<(), String> {
    let downloaded = std::path::Path::new(&downloaded_path);
    if !downloaded.exists() {
        return Err("Downloaded file not found".to_string());
    }

    let helper = std::path::Path::new("/usr/lib/aeroftp/aeroftp-update-helper");
    let status = if helper.exists() {
        info!("Installing .rpm update via branded Polkit helper: {}", downloaded_path);
        std::process::Command::new("pkexec")
            .args(["/usr/lib/aeroftp/aeroftp-update-helper", &downloaded_path])
            .status()
    } else {
        info!("Installing .rpm update via generic pkexec: {}", downloaded_path);
        std::process::Command::new("pkexec")
            .args(["rpm", "-U", &downloaded_path])
            .status()
    }
    .map_err(|e| format!("Failed to launch pkexec: {}", e))?;

    if !status.success() {
        return Err(format!("Installation failed (exit code: {:?}). You can install manually with: sudo rpm -U {}",
            status.code(), downloaded_path));
    }

    info!(".rpm installed successfully, restarting...");
    let _ = std::fs::remove_file(downloaded);

    // Restart via setsid-detached process — survives parent exit
    #[cfg(unix)]
    {
        let current_exe = std::env::current_exe()
            .map_err(|e| format!("Cannot find current exe: {}", e))?;
        let exe_path = current_exe.display().to_string();
        spawn_detached_relaunch(&exe_path);
    }
    app.exit(0);

    Ok(())
}

/// Launch Windows installer (.msi or .exe) and exit the app
#[cfg(windows)]
#[tauri::command]
async fn install_windows_update(app: AppHandle, downloaded_path: String) -> Result<(), String> {
    let downloaded = std::path::Path::new(&downloaded_path);
    if !downloaded.exists() {
        return Err("Downloaded file not found".to_string());
    }

    let ext = downloaded.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    info!("Launching Windows installer: {} ({})", downloaded_path, ext);

    match ext.as_str() {
        "msi" => {
            std::process::Command::new("msiexec")
                .args(["/i", &downloaded_path])
                .spawn()
                .map_err(|e| format!("Failed to launch msiexec: {}", e))?;
        }
        "exe" => {
            std::process::Command::new(&downloaded_path)
                .spawn()
                .map_err(|e| format!("Failed to launch installer: {}", e))?;
        }
        _ => return Err(format!("Unknown installer format: .{}", ext)),
    }

    // Give installer a moment to start, then exit
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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
    // Check if already cancelled (batch stop) — bail immediately
    if state.cancel_flag.load(Ordering::Relaxed) {
        return Err("Transfer cancelled by user".to_string());
    }

    let cancel_flag = state.cancel_flag.clone();
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
        path: None,
    });

    let mut ftp_manager = state.ftp_manager.lock().await;
    
    // Get file size first
    let file_size = ftp_manager.get_file_size(&params.remote_path)
        .await
        .unwrap_or(0);
    
    let start_time = Instant::now();
    let mut last_emit_time = Instant::now();
    let mut last_emit_pct = 0u8;

    // Download with progress (throttled: emit every 150ms or 2% delta)
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

            let is_complete = transferred >= file_size && file_size > 0;
            let time_delta = last_emit_time.elapsed().as_millis() >= 150;
            let pct_delta = percentage.saturating_sub(last_emit_pct) >= 2;
            if time_delta || pct_delta || is_complete {
                last_emit_time = Instant::now();
                last_emit_pct = percentage;
                let eta = if speed > 0 && file_size > transferred {
                    ((file_size - transferred) / speed) as u32
                } else {
                    0
                };

                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: filename.clone(),
                    direction: "download".to_string(),
                    message: None,
                    progress: Some(TransferProgress {
                        transfer_id: transfer_id.clone(),
                        filename: filename.clone(),
                        transferred,
                        total: file_size,
                        percentage,
                        speed_bps: speed,
                        eta_seconds: eta,
                        direction: "download".to_string(),
                        total_files: None,
                        path: None,
                    }),
                    path: None,
                });
            }
            !cancel_flag.load(Ordering::Relaxed)
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
                path: None,
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
                path: None,
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
    // Check if already cancelled (batch stop) — bail immediately
    if state.cancel_flag.load(Ordering::Relaxed) {
        return Err("Transfer cancelled by user".to_string());
    }

    let cancel_flag_upload = state.cancel_flag.clone();
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
        path: None,
    });

    let mut ftp_manager = state.ftp_manager.lock().await;
    let start_time = Instant::now();
    let mut last_emit_time_ul = Instant::now();
    let mut last_emit_pct_ul = 0u8;

    // Upload with progress (throttled: emit every 150ms or 2% delta)
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

            let is_complete = transferred >= file_size && file_size > 0;
            let time_delta = last_emit_time_ul.elapsed().as_millis() >= 150;
            let pct_delta = percentage.saturating_sub(last_emit_pct_ul) >= 2;
            if time_delta || pct_delta || is_complete {
                last_emit_time_ul = Instant::now();
                last_emit_pct_ul = percentage;
                let eta = if speed > 0 && file_size > transferred {
                    ((file_size - transferred) / speed) as u32
                } else {
                    0
                };

                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: filename.clone(),
                    direction: "upload".to_string(),
                    message: None,
                    progress: Some(TransferProgress {
                        transfer_id: transfer_id.clone(),
                        filename: filename.clone(),
                        transferred,
                        total: file_size,
                        percentage,
                        speed_bps: speed,
                        eta_seconds: eta,
                        direction: "upload".to_string(),
                        total_files: None,
                        path: None,
                    }),
                    path: None,
                });
            }
            !cancel_flag_upload.load(Ordering::Relaxed)
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
                path: None,
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
                path: None,
            });
            Err(format!("Upload failed: {}", e))
        }
    }
}

/// Check if a file should be skipped during folder download based on the file_exists_action setting.
/// Used for download: source is remote, destination is local filesystem.
pub fn should_skip_file_download(
    action: &str,
    source_modified: Option<chrono::DateTime<chrono::Utc>>,
    source_size: u64,
    dest_meta: &std::fs::Metadata,
) -> bool {
    use chrono::DateTime;
    let dest_size = dest_meta.len();
    let dest_modified: Option<DateTime<chrono::Utc>> = dest_meta
        .modified()
        .ok()
        .map(DateTime::<chrono::Utc>::from);
    const TOLERANCE_SECS: i64 = 2;

    match action {
        "skip" => true,
        "overwrite_if_newer" => {
            // Skip if source is NOT newer than destination
            match (source_modified, dest_modified) {
                (Some(src), Some(dst)) => src.timestamp() <= dst.timestamp() + TOLERANCE_SECS,
                _ => false, // If unknown dates, don't skip (overwrite)
            }
        }
        "overwrite_if_different" | "skip_if_identical" => {
            // Skip if date AND size are the same
            let size_same = source_size == dest_size;
            let date_same = match (source_modified, dest_modified) {
                (Some(src), Some(dst)) => (src.timestamp() - dst.timestamp()).abs() <= TOLERANCE_SECS,
                _ => false,
            };
            size_same && date_same
        }
        _ => false, // "overwrite" or empty → don't skip
    }
}

/// Check if a file should be skipped during folder upload based on the file_exists_action setting.
/// Used for upload: source is local filesystem, destination is remote.
fn should_skip_file_upload(
    action: &str,
    local_meta: &std::fs::Metadata,
    remote_size: u64,
    remote_modified: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    use chrono::DateTime;
    let local_size = local_meta.len();
    let local_modified: Option<DateTime<chrono::Utc>> = local_meta
        .modified()
        .ok()
        .map(DateTime::<chrono::Utc>::from);
    const TOLERANCE_SECS: i64 = 2;

    match action {
        "skip" => true,
        "overwrite_if_newer" => {
            // Skip if local (source) is NOT newer than remote (dest)
            match (local_modified, remote_modified) {
                (Some(src), Some(dst)) => src.timestamp() <= dst.timestamp() + TOLERANCE_SECS,
                _ => false,
            }
        }
        "overwrite_if_different" | "skip_if_identical" => {
            let size_same = local_size == remote_size;
            let date_same = match (local_modified, remote_modified) {
                (Some(src), Some(dst)) => (src.timestamp() - dst.timestamp()).abs() <= TOLERANCE_SECS,
                _ => false,
            };
            size_same && date_same
        }
        _ => false,
    }
}

#[tauri::command]
async fn download_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    params: DownloadFolderParams
) -> Result<String, String> {
    
    info!("Downloading folder: {} -> {}", params.remote_path, params.local_path);

    // Reset cancel flag
    state.cancel_flag.store(false, Ordering::Relaxed);

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
        path: Some(params.remote_path.clone()),
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
            path: None,
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
        modified: Option<chrono::DateTime<chrono::Utc>>,
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
                    modified: None,
                });
            } else {
                // Add file to download list
                let modified_dt = file.modified.as_ref().and_then(|s| {
                    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S"))
                        .ok()
                        .map(|ndt| ndt.and_utc())
                });
                items_to_download.push(DownloadItem {
                    remote_path: remote_file_path,
                    local_path: local_file_path,
                    is_dir: false,
                    size: file.size.unwrap_or(0),
                    name: file.name,
                    modified: modified_dt,
                });
            }
            
            scan_counter += 1;
            
            // Emit scan progress every 500ms or every 100 files
            if last_scan_emit.elapsed().as_millis() > 500 || scan_counter % 100 == 0 {
                let files_found = items_to_download.iter().filter(|i| !i.is_dir).count();
                let dirs_found = items_to_download.iter().filter(|i| i.is_dir).count();
                let _ = app.emit("transfer_event", TransferEvent {
                    event_type: "scanning".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "download".to_string(),
                    message: Some(format!("Scanning... {} files, {} folders found", files_found, dirs_found)),
                    progress: None,
                    path: None,
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
        event_type: "scanning".to_string(),
        transfer_id: transfer_id.clone(),
        filename: folder_name.clone(),
        direction: "download".to_string(),
        message: Some(format!("Scan complete: {} files in {} folders", total_files, total_dirs)),
        progress: None,
        path: None,
    });
    
    // Download phase: process all items
    let mut downloaded_files = 0;
    let mut skipped_files = 0u64;
    let mut errors = 0;
    let file_exists_action = params.file_exists_action.as_str();

    for item in &items_to_download {
        // Check cancel flag before each item
        if state.cancel_flag.load(Ordering::Relaxed) {
            info!("Folder download cancelled by user after {} files", downloaded_files);
            let _ = app.emit("transfer_event", TransferEvent {
                event_type: "cancelled".to_string(),
                transfer_id: transfer_id.clone(),
                filename: folder_name.clone(),
                direction: "download".to_string(),
                message: Some(format!("Download cancelled after {} files", downloaded_files)),
                progress: None,
                path: None,
            });
            // Restore original directory
            let _ = ftp_manager.change_dir(&original_path).await;
            return Ok(format!("Download cancelled after {} files", downloaded_files));
        }

        if item.is_dir {
            // Create local directory
            if let Err(e) = tokio::fs::create_dir_all(&item.local_path).await {
                warn!("Failed to create directory {}: {}", item.local_path.display(), e);
                errors += 1;
            }
        } else {
            // Check if local file exists and should be skipped
            if !file_exists_action.is_empty() && file_exists_action != "overwrite" {
                if let Ok(local_meta) = std::fs::metadata(&item.local_path) {
                    if local_meta.is_file() && should_skip_file_download(file_exists_action, item.modified, item.size, &local_meta) {
                        skipped_files += 1;
                        // Emit file_skip event
                        let _ = app.emit("transfer_event", TransferEvent {
                            event_type: "file_skip".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: item.name.clone(),
                            direction: "download".to_string(),
                            message: Some(format!("Skipped (identical): {}", item.name)),
                            progress: None,
                            path: Some(item.remote_path.clone()),
                        });
                        continue;
                    }
                }
            }

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
                message: Some(format!("Downloading: {}", item.remote_path)),
                progress: Some(TransferProgress {
                    transfer_id: file_transfer_id.clone(),
                    filename: item.name.clone(),
                    transferred: 0,
                    total: item.size,
                    percentage: 0,
                    speed_bps: 0,
                    eta_seconds: 0,
                    direction: "download".to_string(),
                    total_files: None,
                    path: None,
                }),
                path: Some(item.remote_path.clone()),
            });
            
            // Download the file (streaming with real progress)
            let file_name_only = PathBuf::from(&item.remote_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| item.name.clone());

            let dl_app = app.clone();
            let dl_transfer_id = file_transfer_id.clone();
            let dl_filename = item.name.clone();
            let dl_file_size = item.size;
            let dl_start = Instant::now();

            match ftp_manager.download_file_with_progress(
                &file_name_only,
                item.local_path.to_string_lossy().as_ref(),
                |transferred| {
                    let elapsed = dl_start.elapsed().as_secs_f64();
                    let speed = if elapsed > 0.0 { (transferred as f64 / elapsed) as u64 } else { 0 };
                    let pct = if dl_file_size > 0 {
                        ((transferred as f64 / dl_file_size as f64) * 100.0) as u8
                    } else { 0 };
                    let eta = if speed > 0 && dl_file_size > transferred {
                        ((dl_file_size - transferred) / speed) as u32
                    } else { 0 };

                    let _ = dl_app.emit("transfer_event", TransferEvent {
                        event_type: "progress".to_string(),
                        transfer_id: dl_transfer_id.clone(),
                        filename: dl_filename.clone(),
                        direction: "download".to_string(),
                        message: None,
                        progress: Some(TransferProgress {
                            transfer_id: dl_transfer_id.clone(),
                            filename: dl_filename.clone(),
                            transferred,
                            total: dl_file_size,
                            percentage: pct,
                            speed_bps: speed,
                            eta_seconds: eta,
                            direction: "download".to_string(),
                            total_files: None,
                            path: None,
                        }),
                        path: None,
                    });
                    true // no cancel from sync (uses cancel_flag directly)
                }
            ).await {
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
                            total_files: None,
                            path: None,
                        }),
                        path: Some(item.remote_path.clone()),
                    });

                    // Emit folder progress event (for folder row counter in queue)
                    let _ = app.emit("transfer_event", TransferEvent {
                        event_type: "progress".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: folder_name.clone(),
                        direction: "download".to_string(),
                        message: Some(format!("Downloaded {}/{} files", downloaded_files, total_files)),
                        progress: Some(TransferProgress {
                            transfer_id: transfer_id.clone(),
                            filename: folder_name.clone(),
                            transferred: downloaded_files as u64,
                            total: total_files as u64,
                            percentage,
                            speed_bps: 0,
                            eta_seconds: 0,
                            direction: "download".to_string(),
                            total_files: Some(total_files as u64),
                            path: Some(params.remote_path.clone()),
                        }),
                        path: Some(params.remote_path.clone()),
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
                        path: Some(item.remote_path.clone()),
                    });
                }
            }
        }
    }
    
    // Return to original directory
    let _ = ftp_manager.change_dir(&original_path).await;
    
    // Emit complete event
    let result_message = if errors > 0 && skipped_files > 0 {
        format!("Downloaded {} files, {} skipped, {} errors", downloaded_files, skipped_files, errors)
    } else if skipped_files > 0 {
        format!("Downloaded {} files, {} skipped", downloaded_files, skipped_files)
    } else if errors > 0 {
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
        path: None,
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

    // Reset cancel flag
    state.cancel_flag.store(false, Ordering::Relaxed);

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
        path: Some(params.remote_path.clone()),
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
            path: None,
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
                    event_type: "scanning".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!("Scanning... {} files, {} folders found", files_found, dirs_found)),
                    progress: None,
                    path: None,
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
        event_type: "scanning".to_string(),
        transfer_id: transfer_id.clone(),
        filename: folder_name.clone(),
        direction: "upload".to_string(),
        message: Some(format!("Scan complete: {} files in {} folders", total_files, total_dirs)),
        progress: None,
        path: None,
    });
    
    // ============ PHASE 2: Create all remote directories first ============
    info!("Phase 2: Creating {} remote directories...", total_dirs);
    
    // Sort directories by depth (shortest first) to ensure parent dirs exist
    let mut dirs_sorted = dirs_to_create.clone();
    dirs_sorted.sort_by_key(|a| a.matches('/').count());
    
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
    
    // ============ PHASE 2.5: Collect remote file metadata for smart comparison ============
    let file_exists_action = params.file_exists_action.as_str();
    let mut remote_index: std::collections::HashMap<String, (u64, Option<chrono::DateTime<chrono::Utc>>)> = std::collections::HashMap::new();

    if !file_exists_action.is_empty() && file_exists_action != "overwrite" {
        info!("Phase 2.5: Listing remote files for comparison (action: {})...", file_exists_action);
        for remote_dir in &dirs_sorted {
            // Change to directory and list files
            if ftp_manager.change_dir(remote_dir).await.is_ok() {
                if let Ok(entries) = ftp_manager.list_files().await {
                    for entry in entries {
                        if !entry.is_dir {
                            let remote_file_path = format!("{}/{}", remote_dir.trim_end_matches('/'), entry.name);
                            let modified_dt = entry.modified.as_ref().and_then(|s| {
                                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                                    .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S"))
                                    .ok()
                                    .map(|ndt| ndt.and_utc())
                            });
                            remote_index.insert(remote_file_path, (entry.size.unwrap_or(0), modified_dt));
                        }
                    }
                }
            }
        }
        info!("Phase 2.5 complete: {} remote files indexed for comparison", remote_index.len());
    }

    // ============ PHASE 3: Upload all files with per-file events ============
    info!("Phase 3: Uploading {} files...", total_files);

    let mut uploaded_files = 0u64;
    let mut skipped_files = 0u64;
    let mut errors = 0u64;

    for item in &files_to_upload {
        // Check cancel flag before each file
        if state.cancel_flag.load(Ordering::Relaxed) {
            info!("Folder upload cancelled by user after {} files", uploaded_files);
            let _ = app.emit("transfer_event", TransferEvent {
                event_type: "cancelled".to_string(),
                transfer_id: transfer_id.clone(),
                filename: folder_name.clone(),
                direction: "upload".to_string(),
                message: Some(format!("Upload cancelled after {} files", uploaded_files)),
                progress: None,
                path: None,
            });
            return Ok(format!("Upload cancelled after {} files", uploaded_files));
        }

        // Check if remote file exists and should be skipped
        if !file_exists_action.is_empty() && file_exists_action != "overwrite" {
            if let Some(&(remote_size, remote_modified)) = remote_index.get(&item.remote_path) {
                if let Ok(local_meta) = std::fs::metadata(&item.local_path) {
                    if should_skip_file_upload(file_exists_action, &local_meta, remote_size, remote_modified) {
                        skipped_files += 1;
                        let _ = app.emit("transfer_event", TransferEvent {
                            event_type: "file_skip".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: item.name.clone(),
                            direction: "upload".to_string(),
                            message: Some(format!("Skipped (identical): {}", item.name)),
                            progress: None,
                            path: Some(item.remote_path.clone()),
                        });
                        continue;
                    }
                }
            }
        }

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
                total_files: None,
                path: None,
            }),
            path: Some(item.remote_path.clone()),
        });
        
        info!("Uploading [{}/{}]: {} -> {}",
              uploaded_files + 1, total_files, item.local_path.display(), item.remote_path);

        let ul_app = app.clone();
        let ul_transfer_id = file_transfer_id.clone();
        let ul_filename = item.name.clone();
        let ul_file_size = item.size;
        let ul_start = Instant::now();

        match ftp_manager.upload_file_with_progress(
            item.local_path.to_string_lossy().as_ref(),
            &item.remote_path,
            item.size,
            |transferred| {
                let elapsed = ul_start.elapsed().as_secs_f64();
                let speed = if elapsed > 0.0 { (transferred as f64 / elapsed) as u64 } else { 0 };
                let pct = if ul_file_size > 0 {
                    ((transferred as f64 / ul_file_size as f64) * 100.0) as u8
                } else { 0 };
                let eta = if speed > 0 && ul_file_size > transferred {
                    ((ul_file_size - transferred) / speed) as u32
                } else { 0 };

                let _ = ul_app.emit("transfer_event", TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: ul_transfer_id.clone(),
                    filename: ul_filename.clone(),
                    direction: "upload".to_string(),
                    message: None,
                    progress: Some(TransferProgress {
                        transfer_id: ul_transfer_id.clone(),
                        filename: ul_filename.clone(),
                        transferred,
                        total: ul_file_size,
                        percentage: pct,
                        speed_bps: speed,
                        eta_seconds: eta,
                        direction: "upload".to_string(),
                        total_files: None,
                        path: None,
                    }),
                    path: None,
                });
                true // no cancel from sync
            }
        ).await {
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
                        total_files: None,
                        path: None,
                    }),
                    path: Some(item.remote_path.clone()),
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
                        total_files: Some(total_files as u64),
                        path: Some(remote_base_path.clone()),
                    }),
                    path: Some(remote_base_path.clone()),
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
                    path: Some(item.remote_path.clone()),
                });
            }
        }
    }

    // Emit complete event
    let result_message = if errors > 0 && skipped_files > 0 {
        format!("Uploaded {} files, {} skipped, {} errors", uploaded_files, skipped_files, errors)
    } else if skipped_files > 0 {
        format!("Uploaded {} files, {} skipped", uploaded_files, skipped_files)
    } else if errors > 0 {
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
        path: None,
    });
    
    Ok(result_message)
}

#[tauri::command]
async fn cancel_transfer(
    state: State<'_, AppState>,
    provider_state: State<'_, provider_commands::ProviderState>,
) -> Result<(), String> {
    // Set cancel flag on both FTP and provider states
    state.cancel_flag.store(true, Ordering::Relaxed);
    {
        let mut cancel = provider_state.cancel_flag.lock().await;
        *cancel = true;
    }
    info!("Transfer cancellation requested");
    Ok(())
}

#[tauri::command]
async fn reset_cancel_flag(
    state: State<'_, AppState>,
    provider_state: State<'_, provider_commands::ProviderState>,
) -> Result<(), String> {
    state.cancel_flag.store(false, Ordering::Relaxed);
    {
        let mut cancel = provider_state.cancel_flag.lock().await;
        *cancel = false;
    }
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
        DependencyInfo { name: "portable-pty".into(), version: env!("DEP_VERSION_PORTABLE_PTY").into(), category: "Core".into() },
        DependencyInfo { name: "notify".into(), version: env!("DEP_VERSION_NOTIFY").into(), category: "Core".into() },
        DependencyInfo { name: "image".into(), version: env!("DEP_VERSION_IMAGE").into(), category: "Core".into() },
        // Protocols
        DependencyInfo { name: "suppaftp".into(), version: env!("DEP_VERSION_SUPPAFTP").into(), category: "Protocols".into() },
        DependencyInfo { name: "russh".into(), version: env!("DEP_VERSION_RUSSH").into(), category: "Protocols".into() },
        DependencyInfo { name: "russh-sftp".into(), version: env!("DEP_VERSION_RUSSH_SFTP").into(), category: "Protocols".into() },
        DependencyInfo { name: "reqwest".into(), version: env!("DEP_VERSION_REQWEST").into(), category: "Protocols".into() },
        DependencyInfo { name: "quick-xml".into(), version: env!("DEP_VERSION_QUICK_XML").into(), category: "Protocols".into() },
        DependencyInfo { name: "oauth2".into(), version: env!("DEP_VERSION_OAUTH2").into(), category: "Protocols".into() },
        DependencyInfo { name: "native-tls".into(), version: env!("DEP_VERSION_NATIVE_TLS").into(), category: "Protocols".into() },
        // Security
        DependencyInfo { name: "argon2".into(), version: env!("DEP_VERSION_ARGON2").into(), category: "Security".into() },
        DependencyInfo { name: "aes-gcm".into(), version: env!("DEP_VERSION_AES_GCM").into(), category: "Security".into() },
        DependencyInfo { name: "aes-gcm-siv".into(), version: env!("DEP_VERSION_AES_GCM_SIV").into(), category: "Security".into() },
        DependencyInfo { name: "chacha20poly1305".into(), version: env!("DEP_VERSION_CHACHA20POLY1305").into(), category: "Security".into() },
        DependencyInfo { name: "hkdf".into(), version: env!("DEP_VERSION_HKDF").into(), category: "Security".into() },
        DependencyInfo { name: "aes-kw".into(), version: env!("DEP_VERSION_AES_KW").into(), category: "Security".into() },
        DependencyInfo { name: "aes-siv".into(), version: env!("DEP_VERSION_AES_SIV").into(), category: "Security".into() },
        DependencyInfo { name: "scrypt".into(), version: env!("DEP_VERSION_SCRYPT").into(), category: "Security".into() },
        DependencyInfo { name: "ring".into(), version: env!("DEP_VERSION_RING").into(), category: "Security".into() },
        DependencyInfo { name: "secrecy".into(), version: env!("DEP_VERSION_SECRECY").into(), category: "Security".into() },
        DependencyInfo { name: "sha2".into(), version: env!("DEP_VERSION_SHA2").into(), category: "Security".into() },
        DependencyInfo { name: "hmac".into(), version: env!("DEP_VERSION_HMAC").into(), category: "Security".into() },
        DependencyInfo { name: "blake3".into(), version: env!("DEP_VERSION_BLAKE3").into(), category: "Security".into() },
        DependencyInfo { name: "jsonwebtoken".into(), version: env!("DEP_VERSION_JSONWEBTOKEN").into(), category: "Security".into() },
        // Archives
        DependencyInfo { name: "sevenz-rust".into(), version: env!("DEP_VERSION_SEVENZ_RUST").into(), category: "Archives".into() },
        DependencyInfo { name: "zip".into(), version: env!("DEP_VERSION_ZIP").into(), category: "Archives".into() },
        DependencyInfo { name: "tar".into(), version: env!("DEP_VERSION_TAR").into(), category: "Archives".into() },
        DependencyInfo { name: "flate2".into(), version: env!("DEP_VERSION_FLATE2").into(), category: "Archives".into() },
        DependencyInfo { name: "xz2".into(), version: env!("DEP_VERSION_XZ2").into(), category: "Archives".into() },
        DependencyInfo { name: "bzip2".into(), version: env!("DEP_VERSION_BZIP2").into(), category: "Archives".into() },
        DependencyInfo { name: "unrar".into(), version: env!("DEP_VERSION_UNRAR").into(), category: "Archives".into() },
        // Tauri Plugins
        DependencyInfo { name: "tauri-plugin-fs".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_FS").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-dialog".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_DIALOG").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-shell".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_SHELL").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-notification".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_NOTIFICATION").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-log".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_LOG").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-single-instance".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_SINGLE_INSTANCE").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-localhost".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_LOCALHOST").into(), category: "Plugins".into() },
        DependencyInfo { name: "tauri-plugin-autostart".into(), version: env!("DEP_VERSION_TAURI_PLUGIN_AUTOSTART").into(), category: "Plugins".into() },
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
    dep_versions.insert("aes-gcm".into(), env!("DEP_VERSION_AES_GCM").into());
    dep_versions.insert("argon2".into(), env!("DEP_VERSION_ARGON2").into());
    dep_versions.insert("zip".into(), env!("DEP_VERSION_ZIP").into());
    dep_versions.insert("sevenz-rust".into(), env!("DEP_VERSION_SEVENZ_RUST").into());
    dep_versions.insert("quick-xml".into(), env!("DEP_VERSION_QUICK_XML").into());
    dep_versions.insert("oauth2".into(), env!("DEP_VERSION_OAUTH2").into());
    dep_versions.insert("aes-gcm-siv".into(), env!("DEP_VERSION_AES_GCM_SIV").into());
    dep_versions.insert("chacha20poly1305".into(), env!("DEP_VERSION_CHACHA20POLY1305").into());
    dep_versions.insert("hkdf".into(), env!("DEP_VERSION_HKDF").into());
    dep_versions.insert("aes-kw".into(), env!("DEP_VERSION_AES_KW").into());
    dep_versions.insert("aes-siv".into(), env!("DEP_VERSION_AES_SIV").into());
    dep_versions.insert("scrypt".into(), env!("DEP_VERSION_SCRYPT").into());
    dep_versions.insert("blake3".into(), env!("DEP_VERSION_BLAKE3").into());
    dep_versions.insert("native-tls".into(), env!("DEP_VERSION_NATIVE_TLS").into());

    SystemInfo {
        app_version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        os_version: std::env::consts::ARCH.into(),
        arch: std::env::consts::ARCH.into(),
        tauri_version: env!("DEP_VERSION_TAURI").into(),
        rust_version: env!("RUSTC_VERSION").into(),
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
    validate_path(&path)?;
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
            path: entry.path().to_string_lossy().replace('\\', "/"),
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
    validate_path(&path)?;
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open file manager: {}", e))?;
    }
    
    #[cfg(target_os = "windows")]
    {
        // Use /select, for files or plain path for directories
        let normalized = path.replace('/', "\\");
        let metadata = std::fs::metadata(&normalized);
        if metadata.map(|m| m.is_file()).unwrap_or(false) {
            std::process::Command::new("explorer")
                .args(["/select,", &normalized])
                .spawn()
                .map_err(|e| format!("Failed to open file manager: {}", e))?;
        } else {
            std::process::Command::new("explorer")
                .arg(&normalized)
                .spawn()
                .map_err(|e| format!("Failed to open file manager: {}", e))?;
        }
    }
    
    #[cfg(target_os = "macos")]
    {
        let metadata = std::fs::metadata(&path);
        if metadata.map(|m| m.is_file()).unwrap_or(false) {
            // Use -R to reveal file in Finder (selects it)
            std::process::Command::new("open")
                .args(["-R", &path])
                .spawn()
                .map_err(|e| format!("Failed to open file manager: {}", e))?;
        } else {
            std::process::Command::new("open")
                .arg(&path)
                .spawn()
                .map_err(|e| format!("Failed to open file manager: {}", e))?;
        }
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
    
    let file_name = path.split('/').next_back().unwrap_or(&path).to_string();
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
            path: Some(path.clone()),
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
                    path: Some(path.clone()),
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
                    path: Some(path.clone()),
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
            path: Some(path.clone()),
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
            path: None,
        });
        
        // Phase 2: Delete all files with events (cancellable)
        let mut deleted_files = 0u64;
        let mut errors = 0u64;
        let mut cancelled = false;

        for item in &files_to_delete {
            // Check cancel flag before each file
            if state.cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                info!("Folder deletion cancelled by user after {} files", deleted_files);
                break;
            }
            let file_delete_id = format!("{}-file-{}", delete_id, deleted_files);
            
            let _ = app.emit("transfer_event", TransferEvent {
                event_type: "delete_file_start".to_string(),
                transfer_id: file_delete_id.clone(),
                filename: item.name.clone(),
                direction: "remote".to_string(),
                message: Some(format!("Deleting: {}", item.path)),
                progress: None,
                path: Some(item.path.clone()),
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
                        path: Some(item.path.clone()),
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
                        path: Some(item.path.clone()),
                    });
                }
            }
        }
        
        // Phase 3: Delete directories (deepest first - reverse the order!)
        // Directories were added in scan order (parent first), so we need to reverse
        // Skip if cancelled - partial content may remain
        let dirs_reversed: Vec<_> = dirs_to_delete.iter().rev().collect();
        for dir_path in dirs_reversed {
            if state.cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let dir_name = dir_path.split('/').next_back().unwrap_or(dir_path);
            match ftp_manager.remove_dir(dir_path).await {
                Ok(_) => {
                    let _ = app.emit("transfer_event", TransferEvent {
                        event_type: "delete_dir_complete".to_string(),
                        transfer_id: delete_id.clone(),
                        filename: dir_name.to_string(),
                        direction: "remote".to_string(),
                        message: Some(format!("Removed folder: {}", dir_name)),
                        progress: None,
                        path: Some(dir_path.to_string()),
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
        let result_message = if cancelled {
            format!("Deletion cancelled: {} of {} files deleted", deleted_files, total_files)
        } else if errors > 0 {
            format!("Deleted {} files ({} errors), {} folders", deleted_files, errors, total_dirs)
        } else {
            format!("Deleted {} files, {} folders", deleted_files, total_dirs)
        };

        let _ = app.emit("transfer_event", TransferEvent {
            event_type: if cancelled { "delete_cancelled" } else { "delete_complete" }.to_string(),
            transfer_id: delete_id.clone(),
            filename: file_name.clone(),
            direction: "remote".to_string(),
            message: Some(result_message.clone()),
            progress: None,
            path: Some(path.clone()),
        });

        Ok(result_message)
    }
}

/// Delete a local file or folder with detailed event emission for each deleted item.
#[tauri::command]
async fn delete_local_file(app: AppHandle, state: State<'_, AppState>, path: String) -> Result<String, String> {
    validate_path(&path)?;
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
            path: Some(path.clone()),
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
                    path: Some(path.clone()),
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
                    path: Some(path.clone()),
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
            path: Some(path.clone()),
        });
        
        // Phase 1: Collect all files and directories
        struct DeleteItem {
            path: std::path::PathBuf,
            name: String,
        }
        
        let mut files_to_delete: Vec<DeleteItem> = Vec::new();
        let mut dirs_to_delete: Vec<std::path::PathBuf> = Vec::new();
        let mut dirs_to_scan: Vec<std::path::PathBuf> = vec![path_buf.clone()];
        let mut entry_count: u64 = 0;

        while let Some(current_dir) = dirs_to_scan.pop() {
            let mut read_dir = match tokio::fs::read_dir(&current_dir).await {
                Ok(rd) => rd,
                Err(_) => continue,
            };

            while let Ok(Some(entry)) = read_dir.next_entry().await {
                entry_count += 1;
                if entry_count > 1_000_000 {
                    return Err("Directory contains too many entries (max 1,000,000). Use terminal for large deletions.".to_string());
                }

                let entry_path = entry.path();
                let entry_name = entry.file_name().to_string_lossy().to_string();

                // Use symlink_metadata to avoid following symlinks
                let metadata = tokio::fs::symlink_metadata(&entry_path).await
                    .map_err(|e| format!("Failed to read metadata: {}", e))?;
                if metadata.is_symlink() {
                    // For symlinks, delete the link itself, not the target
                    files_to_delete.push(DeleteItem {
                        path: entry_path,
                        name: entry_name,
                    });
                } else if metadata.is_dir() {
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
            path: None,
        });
        
        // Phase 2: Delete all files with events (cancellable)
        let mut deleted_files = 0u64;
        let mut errors = 0u64;
        let mut cancelled = false;
        let mut last_emit = std::time::Instant::now();

        for item in &files_to_delete {
            if state.cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                info!("Local folder deletion cancelled by user after {} files", deleted_files);
                break;
            }
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
                            path: Some(item.path.display().to_string()),
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
                        path: Some(item.path.display().to_string()),
                    });
                }
            }
        }
        
        // Phase 3: Delete directories (deepest first - reverse the order!)
        // Directories were added in scan order (parent first), so we need to reverse
        // to delete children before parents
        let dirs_reversed: Vec<_> = dirs_to_delete.iter().rev().collect();
        for dir_path in dirs_reversed {
            if state.cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
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
                        path: Some(dir_path.display().to_string()),
                    });
                }
                Err(e) => {
                    warn!("Failed to remove directory {:?}: {}", dir_path, e);
                }
            }
        }
        
        // Emit completion
        let result_message = if cancelled {
            format!("Deletion cancelled: {} of {} files deleted", deleted_files, total_files)
        } else if errors > 0 {
            format!("Deleted {} files ({} errors), {} folders", deleted_files, errors, total_dirs)
        } else {
            format!("Deleted {} files, {} folders", deleted_files, total_dirs)
        };

        let _ = app.emit("transfer_event", TransferEvent {
            event_type: if cancelled { "delete_cancelled" } else { "delete_complete" }.to_string(),
            transfer_id: delete_id.clone(),
            filename: file_name.clone(),
            direction: "local".to_string(),
            message: Some(result_message.clone()),
            progress: None,
            path: Some(path.clone()),
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
    validate_path(&from)?;
    validate_path(&to)?;
    // Check for Windows reserved filenames
    #[cfg(windows)]
    {
        let dest_name = std::path::Path::new(&to)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Some(reserved) = windows_acl::check_windows_reserved(&dest_name) {
            return Err(format!("'{}' is a reserved Windows filename and cannot be used", reserved));
        }
    }

    tokio::fs::rename(&from, &to)
        .await
        .map_err(|e| format!("Failed to rename: {}", e))?;

    Ok(())
}

#[tauri::command]
async fn copy_local_file(from: String, to: String) -> Result<(), String> {
    validate_path(&from)?;
    validate_path(&to)?;
    let from_path = std::path::Path::new(&from);
    if !from_path.exists() {
        return Err(format!("Source does not exist: {}", from));
    }
    if from_path.is_dir() {
        // Recursive directory copy
        copy_dir_recursive(from_path, std::path::Path::new(&to), 0).await?;
    } else {
        tokio::fs::copy(&from, &to)
            .await
            .map_err(|e| format!("Failed to copy file: {}", e))?;
    }
    Ok(())
}

async fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path, depth: u32) -> Result<(), String> {
    if depth > 50 {
        return Err("Directory nesting too deep (max 50 levels)".to_string());
    }
    tokio::fs::create_dir_all(dst)
        .await
        .map_err(|e| format!("Failed to create directory: {}", e))?;
    let mut entries = tokio::fs::read_dir(src)
        .await
        .map_err(|e| format!("Failed to read directory: {}", e))?;
    while let Some(entry) = entries.next_entry().await.map_err(|e| format!("Failed to read entry: {}", e))? {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        // Use symlink_metadata to avoid following symlinks
        let metadata = tokio::fs::symlink_metadata(&src_path).await
            .map_err(|e| format!("Failed to read metadata: {}", e))?;
        if metadata.is_symlink() {
            // Skip symlinks for security
            continue;
        }
        if metadata.is_dir() {
            Box::pin(copy_dir_recursive(&src_path, &dst_path, depth + 1)).await?;
        } else {
            tokio::fs::copy(&src_path, &dst_path)
                .await
                .map_err(|e| format!("Failed to copy file: {}", e))?;
        }
    }
    Ok(())
}

#[tauri::command]
async fn create_local_folder(path: String) -> Result<(), String> {
    validate_path(&path)?;
    tokio::fs::create_dir_all(&path)
        .await
        .map_err(|e| format!("Failed to create folder: {}", e))?;
    
    Ok(())
}

#[tauri::command]
async fn read_file_base64(path: String, max_size_mb: Option<u32>) -> Result<String, String> {
    validate_path(&path)?;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    // Size cap to prevent OOM on large files (default 50MB)
    let max_size: u64 = (max_size_mb.unwrap_or(50) as u64) * 1024 * 1024;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| "Failed to read file metadata".to_string())?;
    if metadata.len() > max_size {
        return Err(format!(
            "File too large for preview ({:.1} MB). Max: {} MB",
            metadata.len() as f64 / (1024.0 * 1024.0),
            max_size / (1024 * 1024)
        ));
    }

    let data = tokio::fs::read(&path)
        .await
        .map_err(|_| "Failed to read file".to_string())?;

    Ok(STANDARD.encode(data))
}

/// Calculate checksum (MD5, SHA-1, SHA-256, or SHA-512) for a local file
#[tauri::command]
async fn calculate_checksum(path: String, algorithm: String) -> Result<String, String> {
    validate_path(&path)?;
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
        "sha1" => {
            use sha1::Digest;
            let mut hasher = sha1::Sha1::new();
            let mut buffer = vec![0u8; 64 * 1024];

            loop {
                let bytes_read = file.read(&mut buffer).await
                    .map_err(|e| format!("Failed to read file: {}", e))?;
                if bytes_read == 0 { break; }
                hasher.update(&buffer[..bytes_read]);
            }

            let result = hasher.finalize();
            Ok(hex::encode(result))
        }
        "sha512" => {
            use sha2::{Sha512, Digest};
            let mut hasher = Sha512::new();
            let mut buffer = vec![0u8; 64 * 1024];

            loop {
                let bytes_read = file.read(&mut buffer).await
                    .map_err(|e| format!("Failed to read file: {}", e))?;
                if bytes_read == 0 { break; }
                hasher.update(&buffer[..bytes_read]);
            }

            let result = hasher.finalize();
            Ok(hex::encode(result))
        }
        _ => Err(format!("Unsupported algorithm: {}. Use 'md5', 'sha1', 'sha256', or 'sha512'", algorithm))
    }
}

/// Compress files/folders into a ZIP archive
#[tauri::command]
async fn compress_files(paths: Vec<String>, output_path: String, password: Option<String>, compression_level: Option<i64>) -> Result<String, String> {
    validate_path(&output_path)?;
    for p in &paths {
        validate_path(p)?;
    }

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

        // ZIP Slip protection: reject entries with traversal or absolute paths
        let entry_name = file.name().to_string();
        if entry_name.split('/').chain(entry_name.split('\\')).any(|c| c == "..")
            || entry_name.starts_with('/')
            || entry_name.starts_with('\\')
            || (entry_name.len() > 2 && entry_name.as_bytes().get(1) == Some(&b':'))
        {
            continue;
        }
        let outpath = std::path::Path::new(&actual_output).join(&entry_name);

        if entry_name.ends_with('/') {
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

/// Validate that an archive entry name is safe for extraction.
/// Rejects absolute paths, Windows drive letters, path traversal (`..`),
/// and entries that would escape the destination directory.
fn is_safe_archive_entry(entry_name: &str) -> bool {
    // Reject empty names
    if entry_name.is_empty() {
        return false;
    }
    // Reject absolute paths (Unix and Windows)
    if entry_name.starts_with('/') || entry_name.starts_with('\\') {
        return false;
    }
    // Reject Windows drive letters (e.g. "C:")
    if entry_name.len() >= 2 && entry_name.as_bytes()[1] == b':' {
        return false;
    }
    // Reject path traversal via ".." in any component (handles both / and \ separators)
    if entry_name.split('/').chain(entry_name.split('\\')).any(|c| c == "..") {
        return false;
    }
    // Reject null bytes
    if entry_name.contains('\0') {
        return false;
    }
    true
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
    use std::fs::{self, File};
    use std::io::BufReader;
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

    let file = File::open(&archive_path)
        .map_err(|e| format!("Failed to open 7z archive: {}", e))?;
    let len = file.metadata()
        .map_err(|e| format!("Failed to get archive metadata: {}", e))?
        .len();
    let reader = BufReader::new(file);

    let pwd = secret_password
        .as_ref()
        .map(|p| Password::from(p.expose_secret()))
        .unwrap_or_else(Password::empty);

    let mut archive = SevenZReader::new(reader, len, pwd)
        .map_err(|e| format!("Failed to read 7z archive: {}", e))?;

    let dest = Path::new(&final_output_dir);

    // C5: Extract entries with per-entry path traversal validation
    // instead of using decompress_file() which extracts blindly
    archive.for_each_entries(|entry, reader| {
        let name = entry.name();

        // Skip entries with unsafe paths (traversal, absolute, drive letters)
        if !is_safe_archive_entry(name) {
            return Ok(true); // skip this entry, continue to next
        }

        let out_path = dest.join(name);

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if entry.is_directory() {
            fs::create_dir_all(&out_path)?;
        } else {
            let mut outfile = File::create(&out_path)?;
            std::io::copy(reader, &mut outfile)?;
        }

        Ok(true) // continue
    }).map_err(|e| format!("Failed to extract 7z archive: {}", e))?;

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

    let reader: Box<dyn std::io::Read> = if ext.ends_with(".tar.gz") || ext.ends_with(".tgz") {
        Box::new(flate2::read::GzDecoder::new(file))
    } else if ext.ends_with(".tar.xz") || ext.ends_with(".txz") {
        Box::new(xz2::read::XzDecoder::new(file))
    } else if ext.ends_with(".tar.bz2") || ext.ends_with(".tbz2") {
        Box::new(bzip2::read::BzDecoder::new(file))
    } else if ext.ends_with(".tar") {
        Box::new(file)
    } else {
        return Err(format!("Unrecognized archive format: {}", ext));
    };

    let mut ar = tar::Archive::new(reader);

    // C5: Iterate entries manually with path traversal validation
    // instead of using unpack() which extracts blindly
    for entry_result in ar.entries().map_err(|e| format!("Failed to read tar entries: {}", e))? {
        let mut entry = entry_result.map_err(|e| format!("Failed to read tar entry: {}", e))?;

        let entry_path = entry.path()
            .map_err(|e| format!("Failed to get entry path: {}", e))?
            .to_string_lossy()
            .to_string();

        // Skip entries with unsafe paths (traversal, absolute, drive letters)
        if !is_safe_archive_entry(&entry_path) {
            continue;
        }

        let out_path = final_output.join(&entry_path);

        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| format!("Failed to create directory '{}': {}", entry_path, e))?;
        } else {
            // Ensure parent directory exists
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent dir for '{}': {}", entry_path, e))?;
            }

            let mut outfile = File::create(&out_path)
                .map_err(|e| format!("Failed to create file '{}': {}", entry_path, e))?;
            std::io::copy(&mut entry, &mut outfile)
                .map_err(|e| format!("Failed to extract '{}': {}", entry_path, e))?;
        }
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
        let entry_name = header.entry().filename.to_string_lossy().to_string();

        // C5: Skip entries with unsafe paths (traversal, absolute, drive letters)
        if !is_safe_archive_entry(&entry_name) {
            archive = header.skip()
                .map_err(|e| format!("Failed to skip RAR entry: {}", e))?;
            continue;
        }

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
async fn read_local_file(path: String, max_size_mb: Option<u32>) -> Result<String, String> {
    validate_path(&path)?;
    // Size cap to prevent OOM on large text files (default 10MB)
    let max_size: u64 = (max_size_mb.unwrap_or(10) as u64) * 1024 * 1024;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| "Failed to read file metadata".to_string())?;
    if metadata.len() > max_size {
        return Err(format!(
            "File too large for text preview ({:.1} MB). Max: {} MB",
            metadata.len() as f64 / (1024.0 * 1024.0),
            max_size / (1024 * 1024)
        ));
    }

    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| e.to_string())?;

    // Detect binary content (null bytes in first 8KB)
    let check_len = bytes.len().min(8192);
    let null_count = bytes[..check_len].iter().filter(|&&b| b == 0).count();
    if null_count > 0 {
        return Err("Binary file detected (contains null bytes). Use read_file_base64 for binary files.".to_string());
    }

    String::from_utf8(bytes)
        .map_err(|_| "File contains invalid UTF-8. Use read_file_base64 for binary files.".to_string())
}

#[tauri::command]
async fn read_local_file_base64(path: String, max_size_mb: Option<u32>) -> Result<String, String> {
    validate_path(&path)?;
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
    
    ftp_manager.download_file_with_progress(&path, &temp_path_str, |_| true)
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

// ============ Favicon Detection ============

/// Parse manifest.json/site.webmanifest to find the best icon path
fn parse_manifest_icons(json_bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(json_bytes).ok()?;
    let icons = value.get("icons")?.as_array()?;

    // Find best icon: prefer PNG ≥48px, fallback to first available
    let mut best: Option<(String, u32)> = None;
    for icon in icons {
        let src = match icon.get("src").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => continue,
        };
        // Parse sizes like "48x48", "192x192"
        let size = icon.get("sizes")
            .and_then(|s| s.as_str())
            .and_then(|s| s.split('x').next())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);

        let is_png = src.ends_with(".png") || icon.get("type").and_then(|t| t.as_str()).is_some_and(|t| t.contains("png"));

        match &best {
            None => best = Some((src.to_string(), size)),
            Some((_, best_size)) => {
                // Prefer sizes between 48-192, favor PNG
                if ((48..=192).contains(&size) && (!(48..=192).contains(best_size) || (is_png && size >= *best_size)))
                    || (*best_size == 0 && size > 0) {
                    best = Some((src.to_string(), size));
                }
            }
        }
    }

    best.map(|(src, _)| src)
}

/// Guess MIME type from file extension (SVG rejected for XSS safety)
fn guess_mime_from_path(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if lower.ends_with(".svg") { return None; } // Reject SVG — can contain <script>
    if lower.ends_with(".png") { Some("image/png") }
    else if lower.ends_with(".ico") { Some("image/x-icon") }
    else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") { Some("image/jpeg") }
    else if lower.ends_with(".webp") { Some("image/webp") }
    else if lower.ends_with(".gif") { Some("image/gif") }
    else { Some("image/png") }
}

/// Validate image magic bytes (defense-in-depth against content spoofing)
fn is_valid_image_magic(bytes: &[u8]) -> bool {
    if bytes.len() < 4 { return false; }
    // PNG
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) { return true; }
    // JPEG
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) { return true; }
    // ICO / CUR
    if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) || bytes.starts_with(&[0x00, 0x00, 0x02, 0x00]) { return true; }
    // GIF
    if bytes.starts_with(b"GIF8") { return true; }
    // WebP
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" { return true; }
    false
}

/// Convert bytes to base64 data URL
fn bytes_to_data_url(bytes: &[u8], mime: &str) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    format!("data:{};base64,{}", mime, STANDARD.encode(bytes))
}

/// Resolve an icon path from manifest relative to the base directory.
/// In FTP context, "/" in manifest means the web root (= base), not the FTP root.
fn resolve_icon_path(base: &str, icon_src: &str) -> Option<String> {
    if icon_src.starts_with("http://") || icon_src.starts_with("https://") {
        return None; // Can't download absolute URLs via FTP
    }
    // Reject path traversal, null bytes, and SVG
    if icon_src.contains("..") || icon_src.contains('\0') {
        return None;
    }
    let lower = icon_src.to_lowercase();
    if lower.ends_with(".svg") {
        return None;
    }
    let prefix = base.trim_end_matches('/');
    let clean_src = icon_src.trim_start_matches('/');
    if clean_src.is_empty() {
        return None;
    }
    if prefix.is_empty() {
        Some(format!("/{}", clean_src))
    } else {
        Some(format!("{}/{}", prefix, clean_src))
    }
}

/// Build path for a file in a base directory
fn make_path(base: &str, filename: &str) -> String {
    let prefix = base.trim_end_matches('/');
    if prefix.is_empty() {
        format!("/{}", filename)
    } else {
        format!("{}/{}", prefix, filename)
    }
}

/// Detect favicon from FTP server using the project's remote root path.
/// Uses SIZE command (control channel only) to check file existence before
/// downloading, preventing FTP data connection corruption on 550 errors.
/// Times out after 10 seconds to avoid holding the FTP mutex too long.
#[tauri::command]
async fn detect_server_favicon(
    state: State<'_, AppState>,
    search_paths: Vec<String>,
) -> Result<Option<String>, String> {
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
    let mut ftp_manager = state.ftp_manager.lock().await;

    for base in &search_paths {
        // 1) Try favicon.ico first (most common, single download)
        let favicon_path = make_path(base, "favicon.ico");
        let favicon_size = ftp_manager.get_file_size(&favicon_path).await.unwrap_or(0);
        if favicon_size > 0 && favicon_size <= 500_000 {
            if let Ok(bytes) = ftp_manager.download_to_bytes(&favicon_path).await {
                if !bytes.is_empty() && is_valid_image_magic(&bytes) {
                    return Ok(Some(bytes_to_data_url(&bytes, "image/x-icon")));
                }
            }
        }

        // 2) Fallback: manifest.json / site.webmanifest → parse icon
        for name in &["manifest.json", "site.webmanifest"] {
            let manifest_path = make_path(base, name);
            let manifest_size = ftp_manager.get_file_size(&manifest_path).await.unwrap_or(0);
            if manifest_size == 0 || manifest_size > 500_000 { continue; }

            if let Ok(manifest_bytes) = ftp_manager.download_to_bytes(&manifest_path).await {
                if manifest_bytes.is_empty() { continue; }
                if let Some(icon_src) = parse_manifest_icons(&manifest_bytes) {
                    if let Some(icon_full) = resolve_icon_path(base, &icon_src) {
                        let icon_size = ftp_manager.get_file_size(&icon_full).await.unwrap_or(0);
                        if icon_size > 0 && icon_size <= 500_000 {
                            if let Ok(icon_bytes) = ftp_manager.download_to_bytes(&icon_full).await {
                                if !icon_bytes.is_empty() && is_valid_image_magic(&icon_bytes) {
                                    if let Some(mime) = guess_mime_from_path(&icon_full) {
                                        return Ok(Some(bytes_to_data_url(&icon_bytes, mime)));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(None)
    }).await;
    match result {
        Ok(inner) => inner,
        Err(_) => Ok(None), // Timeout — no favicon found
    }
}

/// Detect favicon from SFTP/provider server using the project's remote root path.
/// Times out after 10 seconds to avoid holding the provider mutex too long.
#[tauri::command]
async fn detect_provider_favicon(
    state: State<'_, provider_commands::ProviderState>,
    search_paths: Vec<String>,
) -> Result<Option<String>, String> {
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
    let mut provider_lock = state.provider.lock().await;
    let provider: &mut Box<dyn providers::StorageProvider> = provider_lock.as_mut()
        .ok_or("Not connected to any provider")?;

    for base in &search_paths {
        // 1) Try favicon.ico first
        let favicon_path = make_path(base, "favicon.ico");
        if let Ok(favicon_bytes) = provider.download_to_bytes(&favicon_path).await {
            if !favicon_bytes.is_empty() && favicon_bytes.len() <= 500_000 && is_valid_image_magic(&favicon_bytes) {
                return Ok(Some(bytes_to_data_url(&favicon_bytes, "image/x-icon")));
            }
        }

        // 2) Fallback: manifest.json / site.webmanifest → parse icon
        for name in &["manifest.json", "site.webmanifest"] {
            let manifest_path = make_path(base, name);
            if let Ok(manifest_bytes) = provider.download_to_bytes(&manifest_path).await {
                if manifest_bytes.is_empty() || manifest_bytes.len() > 500_000 { continue; }
                if let Some(icon_src) = parse_manifest_icons(&manifest_bytes) {
                    if let Some(icon_full) = resolve_icon_path(base, &icon_src) {
                        if let Ok(icon_bytes) = provider.download_to_bytes(&icon_full).await {
                            if !icon_bytes.is_empty() && icon_bytes.len() <= 500_000 && is_valid_image_magic(&icon_bytes) {
                                if let Some(mime) = guess_mime_from_path(&icon_full) {
                                    return Ok(Some(bytes_to_data_url(&icon_bytes, mime)));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(None)
    }).await;
    match result {
        Ok(inner) => inner,
        Err(_) => Ok(None), // Timeout — no favicon found
    }
}

#[tauri::command]
async fn save_local_file(path: String, content: String) -> Result<(), String> {
    validate_path(&path)?;

    // Additional hardened validation (M63: match ai_tools validate_path level)
    let normalized = path.replace('\\', "/");
    for component in normalized.split('/') {
        if component == ".." {
            return Err("Path traversal ('..') not allowed".to_string());
        }
    }
    let resolved = std::fs::canonicalize(&path).or_else(|_| {
        std::path::Path::new(&path)
            .parent()
            .map(std::fs::canonicalize)
            .unwrap_or(Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no parent")))
    });
    if let Ok(canonical) = resolved {
        let s = canonical.to_string_lossy();
        let denied = ["/proc", "/sys", "/dev", "/boot", "/root",
                      "/etc/shadow", "/etc/passwd", "/etc/ssh", "/etc/sudoers"];
        if denied.iter().any(|d| s.starts_with(d)) {
            return Err(format!("Access to system path denied: {}", s));
        }
    }

    // Atomic write: temp file + rename prevents corruption on crash/power-loss (M35)
    let target = std::path::Path::new(&path);
    let parent = target.parent()
        .ok_or_else(|| "Cannot determine parent directory".to_string())?;
    let tmp_path = parent.join(format!(".aeroftp_save_{}.tmp", chrono::Utc::now().timestamp_millis()));
    tokio::fs::write(&tmp_path, &content)
        .await
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            format!("Failed to write temp file: {}", e)
        })?;
    tokio::fs::rename(&tmp_path, &path)
        .await
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            format!("Failed to finalize file save: {}", e)
        })?;

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
    ftp_manager.upload_file_with_progress(&temp_path_str, &path, content.len() as u64, |_| true)
        .await
        .map_err(|e| format!("Failed to upload file: {}", e))?;
    
    // Clean up temp file
    let _ = tokio::fs::remove_file(&temp_path).await;
    
    Ok(())
}

// ============ Splash Screen ============

/// Global flag: set to true once app_ready has run, so the safety timeout
/// does not re-show the main window after the user has already closed it.
static APP_READY_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Timestamp when splash screen was created — used to enforce minimum display time.
static SPLASH_CREATED_AT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Called by the frontend when React has finished initializing.
/// Closes the splash screen, sets the app menu (deferred from setup to
/// prevent GTK menu flash on the borderless splash), and shows the main window.
#[tauri::command]
async fn app_ready(app: AppHandle) {
    // IMPORTANT: Do NOT set APP_READY_DONE here! Setting it early creates a race
    // condition: rebuild_menu sees the flag, calls app.set_menu() globally, and GTK
    // applies it to the splash window that hasn't been destroyed yet → menu flash.
    // The flag is set at the very END, after splash is dead and menu is installed.

    // 0. Enforce minimum splash display time (2s) so users can read version/license
    //    and the window has time to fully render even on fast machines / Wayland.
    const MIN_SPLASH_SECS: f64 = 2.0;
    if let Some(created) = SPLASH_CREATED_AT.get() {
        let elapsed = created.elapsed().as_secs_f64();
        if elapsed < MIN_SPLASH_SECS {
            let remaining = std::time::Duration::from_secs_f64(MIN_SPLASH_SECS - elapsed);
            info!("Splash minimum wait: {remaining:?}");
            tokio::time::sleep(remaining).await;
        }
    }

    // 1. Close splash — GTK window destruction is async, takes ~500ms
    if let Some(splash) = app.get_webview_window("splashscreen") {
        let _ = splash.close();
        info!("Splash screen closed");
    }

    // 2. Wait for GTK to fully destroy the splash window.
    // During this wait, rebuild_menu still sees APP_READY_DONE==false and defers.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    // 3. Splash is dead — safe to set the global app menu
    if let Some(deferred) = app.try_state::<std::sync::Mutex<Option<tauri::menu::Menu<tauri::Wry>>>>() {
        if let Ok(mut guard) = deferred.lock() {
            if let Some(menu) = guard.take() {
                let _ = app.set_menu(menu);
                info!("App menu set (deferred)");
            }
        }
    }

    // 4. Show main window without menu (frontend controls visibility via toggle_menu_bar)
    if let Some(main_window) = app.get_webview_window("main") {
        let _ = main_window.remove_menu();
        let _ = main_window.show();
        let _ = main_window.set_focus();
        info!("Main window shown");
    }

    // 5. LAST: set the flag so rebuild_menu can freely call app.set_menu()
    // and the safety timeout knows not to fire.
    APP_READY_DONE.store(true, Ordering::SeqCst);
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

#[tauri::command]
fn rebuild_menu(app: AppHandle, labels: std::collections::HashMap<String, String>) -> Result<(), String> {
    use tauri::menu::{Menu, MenuItem, Submenu, PredefinedMenuItem};

    let accel = |shortcut: &'static str| -> Option<&'static str> {
        #[cfg(target_os = "linux")]
        {
            let _ = shortcut;
            None
        }
        #[cfg(not(target_os = "linux"))]
        {
            Some(shortcut)
        }
    };

    let get = |key: &str, fallback: &str| -> String {
        labels.get(key).cloned().unwrap_or_else(|| fallback.to_string())
    };

    let quit = MenuItem::with_id(&app, "quit", get("quit", "Quit AeroFTP"), true, accel("CmdOrCtrl+Q"))
        .map_err(|e| e.to_string())?;
    let about = MenuItem::with_id(&app, "about", get("about", "About AeroFTP"), true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let settings = MenuItem::with_id(&app, "settings", get("settings", "Settings..."), true, accel("CmdOrCtrl+,"))
        .map_err(|e| e.to_string())?;
    let refresh = MenuItem::with_id(&app, "refresh", get("refresh", "Refresh"), true, accel("CmdOrCtrl+R"))
        .map_err(|e| e.to_string())?;
    let shortcuts = MenuItem::with_id(&app, "shortcuts", get("shortcuts", "Keyboard Shortcuts"), true, accel("F1"))
        .map_err(|e| e.to_string())?;
    let support = MenuItem::with_id(&app, "support", get("support", "Support Development"), true, None::<&str>)
        .map_err(|e| e.to_string())?;

    let file_menu = Submenu::with_items(
        &app,
        get("file", "File"),
        true,
        &[
            &MenuItem::with_id(&app, "new_folder", get("newFolder", "New Folder"), true, accel("CmdOrCtrl+N"))
                .map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &settings,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &MenuItem::with_id(&app, "toggle_debug_mode", get("debugMode", "Debug Mode"), true, accel("CmdOrCtrl+Shift+F12"))
                .map_err(|e| e.to_string())?,
            &MenuItem::with_id(&app, "show_dependencies", get("dependencies", "Dependencies..."), true, None::<&str>)
                .map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &quit,
        ],
    ).map_err(|e| e.to_string())?;

    let edit_menu = Submenu::with_items(
        &app,
        get("edit", "Edit"),
        true,
        &[
            &PredefinedMenuItem::undo(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::redo(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::cut(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::copy(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::paste(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::select_all(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &MenuItem::with_id(&app, "rename", get("rename", "Rename"), true, accel("F2"))
                .map_err(|e| e.to_string())?,
            &MenuItem::with_id(&app, "delete", get("delete", "Delete"), true, accel("Delete"))
                .map_err(|e| e.to_string())?,
        ],
    ).map_err(|e| e.to_string())?;

    let devtools_submenu = Submenu::with_items(
        &app,
        get("devtools", "DevTools"),
        true,
        &[
            &MenuItem::with_id(&app, "toggle_devtools", get("toggleDevtools", "Toggle DevTools"), true, accel("CmdOrCtrl+Shift+D"))
                .map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &MenuItem::with_id(&app, "toggle_editor", get("toggleEditor", "Toggle Editor"), true, accel("CmdOrCtrl+1"))
                .map_err(|e| e.to_string())?,
            &MenuItem::with_id(&app, "toggle_terminal", get("toggleTerminal", "Toggle Terminal"), true, accel("CmdOrCtrl+2"))
                .map_err(|e| e.to_string())?,
            &MenuItem::with_id(&app, "toggle_agent", get("toggleAgent", "Toggle Agent"), true, accel("CmdOrCtrl+3"))
                .map_err(|e| e.to_string())?,
        ],
    ).map_err(|e| e.to_string())?;

    let view_menu = Submenu::with_items(
        &app,
        get("view", "View"),
        true,
        &[
            &refresh,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &MenuItem::with_id(&app, "toggle_theme", get("toggleTheme", "Toggle Theme"), true, accel("CmdOrCtrl+T"))
                .map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &devtools_submenu,
        ],
    ).map_err(|e| e.to_string())?;

    let check_update_item = MenuItem::with_id(&app, "check_update", get("checkForUpdates", "Check for Updates"), true, None::<&str>)
        .map_err(|e| e.to_string())?;

    let help_menu = Submenu::with_items(
        &app,
        get("help", "Help"),
        true,
        &[
            &check_update_item,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &shortcuts,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &support,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &about,
        ],
    ).map_err(|e| e.to_string())?;

    let menu = Menu::with_items(&app, &[&file_menu, &edit_menu, &view_menu, &help_menu])
        .map_err(|e| e.to_string())?;

    // If splash is still open (APP_READY_DONE==false), store menu for later —
    // don't set globally (GTK applies global menus to ALL windows, causing flash).
    if !APP_READY_DONE.load(Ordering::SeqCst) {
        if let Some(deferred) = app.try_state::<std::sync::Mutex<Option<tauri::menu::Menu<tauri::Wry>>>>() {
            if let Ok(mut guard) = deferred.lock() {
                *guard = Some(menu);
            }
        }
    } else {
        app.set_menu(menu).map_err(|e| e.to_string())?;
    }

    // Defense-in-depth: if splash somehow still exists, strip its menu
    if let Some(splash) = app.get_webview_window("splashscreen") {
        let _ = splash.remove_menu();
    }

    Ok(())
}

// ============ Sync Commands ============

use sync::{
    CompareOptions, FileComparison, FileInfo, SyncIndex, SyncJournal,
    VerifyPolicy, VerifyResult, RetryPolicy, SyncErrorInfo,
    CanaryResult, CanarySummary, CanarySampleResult,
    build_comparison_results_with_index, should_exclude,
    load_sync_index, save_sync_index,
    load_sync_journal, save_sync_journal, delete_sync_journal,
    verify_local_file, classify_sync_error,
    select_canary_sample, sign_journal, journal_sig_filename,
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

    validate_path(&local_path)?;
    if remote_path.contains('\0') {
        return Err("Remote path contains null bytes".to_string());
    }

    info!("Comparing directories: local={}, remote={}", local_path, remote_path);

    // Emit scan phase: scanning (both local and remote concurrently)
    let _ = app.emit("sync_scan_progress", serde_json::json!({
        "phase": "local",
        "files_found": 0,
    }));

    // Run local and remote scans concurrently (F2 optimization)
    // Local scan runs on filesystem; remote scan holds FTP lock.
    // tokio::join! runs both futures on the same task but interleaves their I/O waits.
    let local_future = get_local_files_recursive(
        &local_path, &local_path, &options.exclude_patterns,
        options.compare_checksum, Some(&state.cancel_flag),
    );

    let remote_future = async {
        let mut ftp_manager = state.ftp_manager.lock().await;
        get_remote_files_recursive_with_progress(
            &app, &mut ftp_manager, &remote_path, &remote_path,
            &options.exclude_patterns, 0,
            Some(&state.cancel_flag),
        ).await
    };

    let (local_result, remote_result) = tokio::join!(local_future, remote_future);
    let local_files = local_result
        .map_err(|e| format!("Failed to scan local directory: {}", e))?;
    let remote_files = remote_result
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

/// Compute SHA-256 hash of a local file (streaming, 64KB chunks)
async fn compute_sha256(path: &std::path::Path) -> Option<String> {
    use sha2::{Sha256, Digest};
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path).await.ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 65_536];
    loop {
        let n = file.read(&mut buf).await.ok()?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// Scan local directory iteratively and build file info map.
/// When `compare_checksum` is true, computes SHA-256 for each file.
pub async fn get_local_files_recursive(
    base_path: &str,
    _current_path: &str,
    exclude_patterns: &[String],
    compare_checksum: bool,
    cancel_flag: Option<&std::sync::atomic::AtomicBool>,
) -> Result<HashMap<String, FileInfo>, String> {
    let mut files = HashMap::new();
    let base = PathBuf::from(base_path);

    if !base.exists() {
        return Ok(files);
    }

    // Use a stack for iterative traversal instead of recursion
    let mut dirs_to_process = vec![base.clone()];

    while let Some(current_dir) = dirs_to_process.pop() {
        // Check cancellation
        if let Some(flag) = cancel_flag {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(files); // Return partial results
            }
        }
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

            // H22: Use symlink_metadata to avoid following symlinks outside sync root.
            // This returns metadata about the symlink itself, not its target.
            let metadata = tokio::fs::symlink_metadata(&path).await.ok();

            // Skip symlinks entirely to prevent data exfiltration via malicious symlinks
            if metadata.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                continue;
            }

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

            // Compute SHA-256 checksum if requested (only for files, not directories)
            let checksum = if compare_checksum && !is_dir {
                compute_sha256(&path).await
            } else {
                None
            };

            let file_info = FileInfo {
                name: name.clone(),
                path: path.to_string_lossy().to_string(),
                size,
                modified,
                is_dir,
                checksum,
            };

            // P2-1: Cap file index at 1M entries to prevent unbounded memory growth
            if files.len() >= 1_000_000 {
                return Err("File scan exceeded 1,000,000 entries. Consider narrowing the scan scope.".to_string());
            }

            files.insert(relative_path, file_info);

            // Add subdirectories to process
            if is_dir {
                dirs_to_process.push(path);
            }
        }
    }

    Ok(files)
}

/// Parallel local scan: directory traversal is sequential (fast), but SHA-256
/// checksums are computed concurrently using a bounded JoinSet + Semaphore.
/// Falls back to sequential scan when `compare_checksum` is false (no I/O benefit).
pub async fn get_local_files_recursive_parallel(
    base_path: &str,
    exclude_patterns: &[String],
    compare_checksum: bool,
    max_concurrent_hashes: usize,
    cancel_flag: Option<&std::sync::atomic::AtomicBool>,
) -> Result<HashMap<String, FileInfo>, String> {
    let base = PathBuf::from(base_path);
    if !base.exists() {
        return Ok(HashMap::new());
    }

    // Phase 1: Walk the directory tree (sequential — fast, mostly metadata)
    #[allow(clippy::type_complexity)]
    let mut file_entries: Vec<(String, String, u64, Option<chrono::DateTime<chrono::Utc>>, bool)> = Vec::new();
    let mut dirs_to_process = vec![base.clone()];

    while let Some(current_dir) = dirs_to_process.pop() {
        if let Some(flag) = cancel_flag {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
        }
        let mut entries = match tokio::fs::read_dir(&current_dir).await {
            Ok(e) => e,
            Err(_) => continue,
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            let relative_path = path
                .strip_prefix(&base)
                .map(|p| p.to_string_lossy().to_string().replace('\\', "/"))
                .unwrap_or_else(|_| name.clone());

            if should_exclude(&relative_path, exclude_patterns) {
                continue;
            }

            // H22: Use symlink_metadata to avoid following symlinks outside sync root.
            let metadata = tokio::fs::symlink_metadata(&path).await.ok();

            // Skip symlinks entirely to prevent data exfiltration via malicious symlinks
            if metadata.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                continue;
            }

            let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let modified = metadata.as_ref().and_then(|m| {
                m.modified().ok().map(|t| {
                    let datetime: chrono::DateTime<chrono::Utc> = t.into();
                    datetime
                })
            });
            let size = if is_dir { 0 } else { metadata.as_ref().map(|m| m.len()).unwrap_or(0) };
            let abs_path = path.to_string_lossy().to_string();

            // P2-1: Cap file index at 1M entries to prevent unbounded memory growth
            if file_entries.len() >= 1_000_000 {
                return Err("File scan exceeded 1,000,000 entries. Consider narrowing the scan scope.".to_string());
            }

            file_entries.push((relative_path, abs_path, size, modified, is_dir));

            if is_dir {
                dirs_to_process.push(path);
            }
        }
    }

    // Phase 2: Compute checksums in parallel (only when requested)
    let mut files = HashMap::with_capacity(file_entries.len());

    if compare_checksum {
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(
            max_concurrent_hashes.clamp(1, 16),
        ));
        let mut join_set = tokio::task::JoinSet::new();

        for (relative_path, abs_path, size, modified, is_dir) in file_entries {
            if is_dir {
                files.insert(relative_path, FileInfo {
                    name: std::path::Path::new(&abs_path)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    path: abs_path,
                    size,
                    modified,
                    is_dir: true,
                    checksum: None,
                });
                continue;
            }

            let sem = semaphore.clone();
            let path_clone = abs_path.clone();
            let rel_clone = relative_path.clone();

            join_set.spawn(async move {
                let _permit = sem.acquire().await;
                let checksum = compute_sha256(std::path::Path::new(&path_clone)).await;
                (rel_clone, path_clone, size, modified, checksum)
            });
        }

        while let Some(result) = join_set.join_next().await {
            if let Ok((rel_path, abs_path, size, modified, checksum)) = result {
                let name = std::path::Path::new(&abs_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                files.insert(rel_path, FileInfo {
                    name,
                    path: abs_path,
                    size,
                    modified,
                    is_dir: false,
                    checksum,
                });
            }
        }
    } else {
        // No checksums — just convert entries to FileInfo directly
        for (relative_path, abs_path, size, modified, is_dir) in file_entries {
            let name = std::path::Path::new(&abs_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            files.insert(relative_path, FileInfo {
                name,
                path: abs_path,
                size,
                modified,
                is_dir,
                checksum: None,
            });
        }
    }

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
    cancel_flag: Option<&std::sync::atomic::AtomicBool>,
) -> Result<HashMap<String, FileInfo>, String> {
    let mut files = HashMap::new();
    let mut dirs_to_process = vec![base_path.to_string()];

    while let Some(current_dir) = dirs_to_process.pop() {
        // Check cancellation flag — release FTP lock immediately on cancel
        if let Some(flag) = cancel_flag {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                info!("Remote scan cancelled by user after {} files", files.len());
                return Ok(files); // Return partial results (will be discarded by frontend)
            }
        }
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
    validate_path(&local_path)?;
    validate_path(&remote_path)?;
    load_sync_index(&local_path, &remote_path)
}

#[tauri::command]
fn save_sync_index_cmd(index: SyncIndex) -> Result<(), String> {
    validate_path(&index.local_path)?;
    validate_path(&index.remote_path)?;
    save_sync_index(&index)
}

// ============ Sync Journal Commands (Phase 2: Reliability) ============

#[tauri::command]
fn load_sync_journal_cmd(local_path: String, remote_path: String) -> Result<Option<SyncJournal>, String> {
    validate_path(&local_path)?;
    validate_path(&remote_path)?;
    load_sync_journal(&local_path, &remote_path)
}

#[tauri::command]
fn save_sync_journal_cmd(journal: SyncJournal) -> Result<(), String> {
    validate_path(&journal.local_path)?;
    validate_path(&journal.remote_path)?;
    save_sync_journal(&journal)
}

#[tauri::command]
fn delete_sync_journal_cmd(local_path: String, remote_path: String) -> Result<(), String> {
    validate_path(&local_path)?;
    validate_path(&remote_path)?;
    delete_sync_journal(&local_path, &remote_path)
}

#[tauri::command]
fn list_sync_journals_cmd() -> Result<Vec<sync::JournalSummary>, String> {
    sync::list_sync_journals()
}

#[tauri::command]
fn cleanup_old_journals_cmd(max_age_days: u32) -> Result<u32, String> {
    sync::cleanup_old_journals(max_age_days)
}

#[tauri::command]
fn clear_all_journals_cmd() -> Result<u32, String> {
    sync::clear_all_journals()
}

#[tauri::command]
fn load_sync_profiles_cmd() -> Result<Vec<sync::SyncProfile>, String> {
    sync::load_sync_profiles()
}

#[tauri::command]
fn save_sync_profile_cmd(profile: sync::SyncProfile) -> Result<(), String> {
    sync::save_sync_profile(&profile)
}

#[tauri::command]
fn delete_sync_profile_cmd(id: String) -> Result<(), String> {
    sync::delete_sync_profile(&id)
}

// ─── Phase 3A+ Commands: Parallel Scan, Scheduler, Watcher ─────────────

#[tauri::command]
async fn get_parallel_scan_files(
    base_path: String,
    exclude_patterns: Vec<String>,
    compare_checksum: bool,
    max_concurrent_hashes: Option<usize>,
) -> Result<HashMap<String, FileInfo>, String> {
    validate_path(&base_path)?;
    let concurrency = max_concurrent_hashes.unwrap_or(4);
    get_local_files_recursive_parallel(
        &base_path,
        &exclude_patterns,
        compare_checksum,
        concurrency,
        None,
    ).await
}

#[tauri::command]
fn get_sync_schedule_cmd() -> Result<sync_scheduler::SyncSchedule, String> {
    Ok(sync_scheduler::load_sync_schedule())
}

#[tauri::command]
fn save_sync_schedule_cmd(schedule: sync_scheduler::SyncSchedule) -> Result<(), String> {
    sync_scheduler::save_sync_schedule(&schedule)
}

#[tauri::command]
fn get_watcher_status_cmd(watch_path: Option<String>) -> Result<serde_json::Value, String> {
    // Validate the watch path if provided
    if let Some(ref p) = watch_path {
        filesystem::validate_path(p)?;
    }

    // Returns a snapshot of the filesystem watcher status
    // Watcher lifecycle is managed by background_sync_worker, not directly from frontend
    let native_backend = if cfg!(target_os = "linux") { "inotify" }
        else if cfg!(target_os = "macos") { "fsevent" }
        else if cfg!(target_os = "windows") { "readirectorychanges" }
        else { "poll" };

    let inotify_info = if cfg!(target_os = "linux") {
        watch_path.as_ref().map(|p| {
            let (count, should_warn, should_fallback) =
                file_watcher::check_inotify_capacity(std::path::Path::new(p));
            serde_json::json!({
                "subdirectory_count": count,
                "should_warn": should_warn,
                "should_fallback_to_poll": should_fallback,
            })
        })
    } else {
        None
    };

    Ok(serde_json::json!({
        "available": true,
        "native_backend": native_backend,
        "inotify_capacity": inotify_info,
    }))
}

/// Get transfer optimization hints for the current cloud provider
#[tauri::command]
fn get_transfer_optimization_hints(provider_type: Option<String>) -> Result<providers::TransferOptimizationHints, String> {
    // Provider type is passed from frontend based on the connected server profile
    let ptype = provider_type.unwrap_or_default();
    let hints = match ptype.to_lowercase().as_str() {
        "sftp" => providers::TransferOptimizationHints {
            supports_resume_download: true,
            supports_resume_upload: true,
            supports_compression: true,
            supports_delta_sync: true,
            ..Default::default()
        },
        "s3" => providers::TransferOptimizationHints {
            supports_multipart: true,
            multipart_threshold: 5 * 1024 * 1024,
            multipart_part_size: 5 * 1024 * 1024,
            multipart_max_parallel: 4,
            supports_server_checksum: true,
            preferred_checksum_algo: Some("ETag".to_string()),
            ..Default::default()
        },
        "ftp" | "ftps" => providers::TransferOptimizationHints {
            supports_resume_download: true,
            supports_resume_upload: true,
            ..Default::default()
        },
        "webdav" => providers::TransferOptimizationHints {
            supports_resume_download: true,
            ..Default::default()
        },
        _ => providers::TransferOptimizationHints::default(),
    };
    Ok(hints)
}

// =============================
// Multi-Path Sync Commands (#52)
// =============================

#[tauri::command]
fn get_multi_path_config() -> sync::MultiPathConfig {
    sync::load_multi_path_config()
}

#[tauri::command]
fn save_multi_path_config_cmd(config: sync::MultiPathConfig) -> Result<(), String> {
    sync::save_multi_path_config(&config)
}

#[tauri::command]
fn add_path_pair(pair: sync::PathPair) -> Result<sync::MultiPathConfig, String> {
    let mut config = sync::load_multi_path_config();
    config.pairs.push(pair);
    sync::save_multi_path_config(&config)?;
    Ok(config)
}

#[tauri::command]
fn remove_path_pair(pair_id: String) -> Result<sync::MultiPathConfig, String> {
    let mut config = sync::load_multi_path_config();
    config.pairs.retain(|p| p.id != pair_id);
    sync::save_multi_path_config(&config)?;
    Ok(config)
}

// =============================
// Sync Template Commands (#153)
// =============================

#[tauri::command]
fn export_sync_template_cmd(
    name: String,
    description: String,
    profile_id: String,
    local_path: String,
    remote_path: String,
    exclude_patterns: Vec<String>,
) -> Result<String, String> {
    let profiles = sync::load_sync_profiles()?;
    let profile = profiles.iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| format!("Profile '{}' not found", profile_id))?;
    let schedule = sync_scheduler::load_sync_schedule();
    let schedule_opt = if schedule.enabled { Some(&schedule) } else { None };
    let template = sync::export_sync_template(
        &name, &description, profile, &local_path, &remote_path, &exclude_patterns, schedule_opt,
    )?;
    serde_json::to_string_pretty(&template).map_err(|e| e.to_string())
}

#[tauri::command]
fn import_sync_template_cmd(json_content: String) -> Result<sync::SyncTemplate, String> {
    let template: sync::SyncTemplate = serde_json::from_str(&json_content)
        .map_err(|e| format!("Invalid template format: {}", e))?;
    if template.schema_version != 1 {
        return Err(format!("Unsupported template version: {}", template.schema_version));
    }
    Ok(template)
}

// =============================
// Rollback Commands (#154)
// =============================

#[tauri::command]
fn create_sync_snapshot_cmd(local_path: String, remote_path: String) -> Result<String, String> {
    let index = sync::load_sync_index(&local_path, &remote_path)?
        .ok_or_else(|| "No sync index found — run sync first".to_string())?;
    let snapshot = sync::create_sync_snapshot(&local_path, &remote_path, &index);
    sync::save_sync_snapshot(&snapshot)?;
    Ok(snapshot.id)
}

#[tauri::command]
fn list_sync_snapshots_cmd() -> Result<Vec<sync::SyncSnapshot>, String> {
    sync::list_sync_snapshots()
}

#[tauri::command]
fn delete_sync_snapshot_cmd(snapshot_id: String) -> Result<(), String> {
    sync::delete_sync_snapshot(&snapshot_id)
}

// =============================
// Delta Sync Commands (#155)
// =============================

/// Analyze a file pair and return delta sync stats (preview, no actual transfer)
#[tauri::command]
async fn delta_sync_analyze(
    local_path: String,
    remote_path: String,
) -> Result<delta_sync::DeltaResult, String> {
    validate_path(&local_path)?;
    validate_path(&remote_path)?;

    // Read local file
    let local_data = tokio::fs::read(&local_path).await
        .map_err(|e| format!("Failed to read local file: {}", e))?;

    if (local_data.len() as u64) < delta_sync::DELTA_MIN_FILE_SIZE {
        return Err(format!("File too small for delta sync ({}B < {}B minimum)",
            local_data.len(), delta_sync::DELTA_MIN_FILE_SIZE));
    }

    // For analysis, we use the local file as both source and simulate
    // In real usage, remote_data would come from provider.read_range()
    let block_size = delta_sync::compute_block_size(local_data.len() as u64);
    let sigs = delta_sync::compute_signatures(&local_data, block_size);

    // Read remote (local copy for now — real impl would use provider)
    let remote_data = tokio::fs::read(&remote_path).await
        .map_err(|e| format!("Failed to read remote file: {}", e))?;

    let (_, result) = delta_sync::compute_delta(&remote_data, &sigs);
    Ok(result)
}

// =============================
// Canary Sync Commands
// =============================

/// Run a canary (sample-based) dry-run sync analysis.
/// Scans local files, selects a percentage-based sample, and projects
/// what a full sync would do without actually transferring anything.
#[tauri::command]
async fn sync_canary_run(
    local_path: String,
    remote_path: String,
    percent: u8,
    selection: String,
) -> Result<CanaryResult, String> {
    validate_path(&local_path)?;
    if remote_path.contains('\0') {
        return Err("Remote path contains null bytes".to_string());
    }

    // Clamp percent to 5-50 range
    let percent = percent.clamp(5, 50);

    // Scan local files (no checksum for speed)
    let exclude_patterns = sync::CompareOptions::default().exclude_patterns;
    let local_files = get_local_files_recursive(
        &local_path,
        &local_path,
        &exclude_patterns,
        false,
        None,
    )
    .await?;

    // Only count non-directory files for sampling
    let total_files = local_files.iter().filter(|(_, f)| !f.is_dir).count();
    if total_files == 0 {
        return Ok(CanaryResult {
            sampled_files: 0,
            total_files: 0,
            results: Vec::new(),
            summary: CanarySummary {
                would_upload: 0,
                would_download: 0,
                would_delete: 0,
                conflicts: 0,
                errors: 0,
                estimated_transfer_size: 0,
            },
        });
    }

    // Calculate sample size: total * percent / 100, minimum 1
    let sample_size = ((total_files as u64 * percent as u64) / 100).max(1) as usize;

    // Select sample based on strategy
    let sample = select_canary_sample(&local_files, sample_size, &selection);

    // Build canary results by analyzing the sample
    // In dry-run mode, local-only files are projected as uploads
    let mut results = Vec::new();
    let mut would_upload: usize = 0;
    let mut would_download: usize = 0;
    let mut would_delete: usize = 0;
    let conflicts: usize = 0;
    let mut estimated_transfer_size: u64 = 0;

    // Load sync index for comparison if available
    let index = load_sync_index(&local_path, &remote_path).ok().flatten();

    for (rel_path, info) in &sample {
        // Determine projected action based on index state
        let action = if let Some(idx) = &index {
            if let Some(cached) = idx.files.get(rel_path) {
                // File exists in index — check if it changed locally
                let local_changed = info.size != cached.size
                    || !sync::timestamps_equal(info.modified, cached.modified);
                if local_changed {
                    "upload" // Changed since last sync
                } else {
                    "skip" // Unchanged
                }
            } else {
                "upload" // New file not in index
            }
        } else {
            "upload" // No index available — assume upload needed
        };

        if action == "skip" {
            continue;
        }

        match action {
            "upload" => {
                would_upload += 1;
                estimated_transfer_size += info.size;
            }
            "download" => {
                would_download += 1;
                estimated_transfer_size += info.size;
            }
            "delete" => {
                would_delete += 1;
            }
            _ => {}
        }

        results.push(CanarySampleResult {
            relative_path: rel_path.clone(),
            action: action.to_string(),
            success: true, // Dry-run always succeeds
            error: None,
            bytes: info.size,
        });
    }

    // Extrapolate totals based on sample ratio
    let sampled_files = sample.len();

    Ok(CanaryResult {
        sampled_files,
        total_files,
        results,
        summary: CanarySummary {
            would_upload,
            would_download,
            would_delete,
            conflicts,
            errors: 0,
            estimated_transfer_size,
        },
    })
}

/// Approve canary results — placeholder that returns a success message.
/// The actual full sync is triggered by the frontend calling `parallel_sync_execute`.
#[tauri::command]
async fn sync_canary_approve() -> Result<String, String> {
    Ok("Canary approved — proceed with full sync".to_string())
}

// =============================
// Signed Audit Log Commands
// =============================

/// Sign an existing sync journal with HMAC-SHA256.
/// Saves the hex-encoded signature as a .sig file alongside the journal.
#[tauri::command]
async fn sign_sync_journal(
    local_path: String,
    remote_path: String,
    signing_key: String,
) -> Result<String, String> {
    validate_path(&local_path)?;
    if remote_path.contains('\0') {
        return Err("Remote path contains null bytes".to_string());
    }

    // Load the journal
    let journal = load_sync_journal(&local_path, &remote_path)?
        .ok_or_else(|| "No sync journal found for this path pair".to_string())?;

    // Decode hex signing key
    let key_bytes = hex::decode(&signing_key)
        .map_err(|e| format!("Invalid hex signing key: {}", e))?;
    if key_bytes.is_empty() {
        return Err("Signing key cannot be empty".to_string());
    }
    if key_bytes.len() < 32 {
        return Err("Signing key must be at least 32 bytes (64 hex chars)".to_string());
    }

    // Compute HMAC-SHA256 signature
    let signature = sign_journal(&journal, &key_bytes)?;

    // Save .sig file alongside the journal
    let journal_dir = dirs::config_dir()
        .ok_or_else(|| "Cannot determine config directory".to_string())?
        .join("aeroftp")
        .join("sync-journal");
    let sig_path = journal_dir.join(journal_sig_filename(&local_path, &remote_path));
    tokio::fs::write(&sig_path, signature.as_bytes())
        .await
        .map_err(|e| format!("Failed to write signature file: {}", e))?;

    Ok(signature)
}

/// Verify an existing journal signature.
/// Returns true if the stored signature matches the recomputed HMAC.
#[tauri::command]
async fn verify_journal_signature(
    local_path: String,
    remote_path: String,
    signing_key: String,
) -> Result<bool, String> {
    validate_path(&local_path)?;
    if remote_path.contains('\0') {
        return Err("Remote path contains null bytes".to_string());
    }

    // Load the journal
    let journal = load_sync_journal(&local_path, &remote_path)?
        .ok_or_else(|| "No sync journal found for this path pair".to_string())?;

    // Read the .sig file
    let journal_dir = dirs::config_dir()
        .ok_or_else(|| "Cannot determine config directory".to_string())?
        .join("aeroftp")
        .join("sync-journal");
    let sig_path = journal_dir.join(journal_sig_filename(&local_path, &remote_path));
    let stored_sig = tokio::fs::read_to_string(&sig_path)
        .await
        .map_err(|e| format!("Failed to read signature file: {}", e))?;

    // Decode hex signing key
    let key_bytes = hex::decode(&signing_key)
        .map_err(|e| format!("Invalid hex signing key: {}", e))?;
    if key_bytes.is_empty() {
        return Err("Signing key cannot be empty".to_string());
    }
    if key_bytes.len() < 32 {
        return Err("Signing key must be at least 32 bytes (64 hex chars)".to_string());
    }

    // Recompute HMAC-SHA256
    let computed_sig = sign_journal(&journal, &key_bytes)?;

    // Constant-time comparison to prevent timing attacks
    let a = computed_sig.as_bytes();
    let b = stored_sig.trim().as_bytes();
    let result = if a.len() != b.len() {
        false
    } else {
        a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    };
    Ok(result)
}

/// Execute sync transfers in parallel using a bounded Semaphore pool.
///
/// Each stream creates its own FTP connection (FTP doesn't support multiplexing).
/// Progress events are emitted per-stream with `stream_id` for UI tracking.
/// The journal is updated atomically after each transfer completes.
#[tauri::command]
async fn parallel_sync_execute(
    app: AppHandle,
    transfers: Vec<transfer_pool::SyncTransferEntry>,
    server_host: String,
    server_user: String,
    server_pass: String,
    max_streams: u8,
) -> Result<transfer_pool::ParallelSyncResult, String> {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // Validate all transfer entry paths before processing
    for entry in &transfers {
        filesystem::validate_path(&entry.local_path)?;
        // remote_path validation: reject null bytes and path traversal
        if entry.remote_path.contains('\0') || entry.remote_path.contains("..") {
            return Err(format!("Invalid remote path: {}", entry.relative_path));
        }
    }

    let start = Instant::now();
    // P2-5: Use validate_config for consistent validation (clamp streams + default timeout)
    let mut pool_config = transfer_pool::ParallelTransferConfig {
        max_streams,
        acquire_timeout_ms: 30000,
    };
    transfer_pool::validate_config(&mut pool_config);
    let max_streams = pool_config.max_streams as usize;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_streams));
    let result = Arc::new(Mutex::new(transfer_pool::ParallelSyncResult::new()));
    let total_count = transfers.len();

    info!(
        "parallel_sync_execute: {} transfers, {} streams, host={}",
        total_count, max_streams, server_host
    );

    // Emit start event
    let _ = app.emit("sync-parallel-progress", serde_json::json!({
        "phase": "start",
        "total": total_count,
        "streams": max_streams,
    }));

    let mut join_set = tokio::task::JoinSet::new();

    for (index, entry) in transfers.into_iter().enumerate() {
        let sem = semaphore.clone();
        let res = result.clone();
        let app_clone = app.clone();
        let host = server_host.clone();
        let user = server_user.clone();
        let pass = server_pass.clone();

        join_set.spawn(async move {
            // Acquire semaphore permit (bounds concurrency)
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    let mut r = res.lock().await;
                    r.errors.push(transfer_pool::ParallelTransferError {
                        relative_path: entry.relative_path.clone(),
                        action: entry.action.clone(),
                        error: "Semaphore closed".to_string(),
                        retryable: false,
                    });
                    return;
                }
            };

            let stream_id = index % 8; // Visual stream assignment

            // Emit per-file start
            let _ = app_clone.emit("sync-parallel-progress", serde_json::json!({
                "phase": "file_start",
                "stream_id": stream_id,
                "relative_path": entry.relative_path,
                "action": entry.action,
                "index": index,
                "total": total_count,
            }));

            // Skip directories (mkdir is handled separately)
            if entry.is_dir {
                if entry.action == transfer_pool::TransferAction::Mkdir {
                    // Create local directory
                    let _ = tokio::fs::create_dir_all(&entry.local_path).await;
                }
                let mut r = res.lock().await;
                r.skipped += 1;
                return;
            }

            // Execute transfer with a dedicated FTP connection
            let transfer_result = execute_single_transfer(
                &host,
                &user,
                &pass,
                &entry,
                &app_clone,
                stream_id,
                index,
                total_count,
            ).await;

            let mut r = res.lock().await;
            match transfer_result {
                Ok(action) => match action.as_str() {
                    "uploaded" => r.uploaded += 1,
                    "downloaded" => r.downloaded += 1,
                    "deleted" => r.deleted += 1,
                    _ => r.skipped += 1,
                },
                Err(e) => {
                    let retryable = sync::classify_sync_error(&e, Some(&entry.relative_path)).retryable;
                    r.errors.push(transfer_pool::ParallelTransferError {
                        relative_path: entry.relative_path.clone(),
                        action: entry.action.clone(),
                        error: e,
                        retryable,
                    });
                }
            }

            // Emit per-file complete
            let _ = app_clone.emit("sync-parallel-progress", serde_json::json!({
                "phase": "file_complete",
                "stream_id": stream_id,
                "relative_path": entry.relative_path,
                "action": entry.action,
                "index": index,
                "total": total_count,
            }));
        });
    }

    // Wait for all transfers to complete, propagating JoinErrors (panics/cancellations)
    while let Some(join_result) = join_set.join_next().await {
        if let Err(join_err) = join_result {
            let mut r = result.lock().await;
            let err_index = r.errors.len();
            r.errors.push(transfer_pool::ParallelTransferError {
                relative_path: format!("task-{}", err_index),
                action: transfer_pool::TransferAction::Upload,
                error: format!("Task panicked: {}", join_err),
                retryable: false,
            });
        }
    }

    let mut final_result = result.lock().await;
    final_result.duration_ms = start.elapsed().as_millis() as u64;
    final_result.streams_used = max_streams as u8;

    let result_clone = final_result.clone();

    info!(
        "parallel_sync_execute complete: ↑{} ↓{} ✗{} skip={} in {}ms using {} streams",
        result_clone.uploaded,
        result_clone.downloaded,
        result_clone.errors.len(),
        result_clone.skipped,
        result_clone.duration_ms,
        result_clone.streams_used,
    );

    // Emit completion
    let _ = app.emit("sync-parallel-progress", serde_json::json!({
        "phase": "complete",
        "uploaded": result_clone.uploaded,
        "downloaded": result_clone.downloaded,
        "errors": result_clone.errors.len(),
        "duration_ms": result_clone.duration_ms,
    }));

    Ok(result_clone)
}

/// Execute a single FTP transfer (upload, download, or delete) with a dedicated connection.
/// Each call creates and tears down its own FTP connection to avoid multiplexing issues.
#[allow(clippy::too_many_arguments)]
async fn execute_single_transfer(
    host: &str,
    user: &str,
    pass: &str,
    entry: &transfer_pool::SyncTransferEntry,
    app: &AppHandle,
    stream_id: usize,
    index: usize,
    total: usize,
) -> Result<String, String> {
    let mut ftp = ftp::FtpManager::new();

    ftp.connect(host).await
        .map_err(|e| format!("Stream {}: connect failed: {}", stream_id, e))?;
    ftp.login(user, pass).await
        .map_err(|e| format!("Stream {}: login failed: {}", stream_id, e))?;

    let result = match entry.action {
        transfer_pool::TransferAction::Upload => {
            // Ensure parent directory exists
            if let Some(parent) = std::path::Path::new(&entry.remote_path).parent() {
                let parent_str = parent.to_string_lossy().to_string();
                if !parent_str.is_empty() && parent_str != "/" {
                    let _ = ftp.mkdir(&parent_str).await; // ignore if exists
                }
            }

            let file_size = tokio::fs::metadata(&entry.local_path)
                .await
                .map(|m| m.len())
                .unwrap_or(entry.expected_size);

            let start_time = Instant::now();
            let app_ref = app.clone();
            let transfer_id = format!("psync-{}-{}", stream_id, index);
            let filename = entry.relative_path.clone();
            let mut last_emit_time_ul = Instant::now();
            let mut last_emit_pct_ul: u8 = 0;

            ftp.upload_file_with_progress(
                &entry.local_path,
                &entry.remote_path,
                file_size,
                move |transferred| {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let speed = if elapsed > 0.0 { (transferred as f64 / elapsed) as u64 } else { 0 };
                    let pct = if file_size > 0 {
                        ((transferred as f64 / file_size as f64) * 100.0) as u8
                    } else { 0 };

                    // Throttle: emit every 150ms or 2% delta (matches standard transfer path)
                    let is_complete = transferred >= file_size && file_size > 0;
                    let time_ok = last_emit_time_ul.elapsed().as_millis() >= 150;
                    let pct_ok = pct.saturating_sub(last_emit_pct_ul) >= 2;
                    if time_ok || pct_ok || is_complete {
                        last_emit_time_ul = Instant::now();
                        last_emit_pct_ul = pct;
                        let _ = app_ref.emit("sync-parallel-progress", serde_json::json!({
                            "phase": "transfer_progress",
                            "stream_id": stream_id,
                            "transfer_id": transfer_id,
                            "relative_path": filename,
                            "direction": "upload",
                            "transferred": transferred,
                            "total": file_size,
                            "percentage": pct,
                            "speed_bps": speed,
                            "index": index,
                            "total_files": total,
                        }));
                    }
                    true // continue
                },
            ).await.map_err(|e| format!("Upload failed: {}", e))?;

            Ok("uploaded".to_string())
        }
        transfer_pool::TransferAction::Download => {
            // Ensure local parent directory exists
            if let Some(parent) = std::path::Path::new(&entry.local_path).parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }

            let file_size = ftp.get_file_size(&entry.remote_path)
                .await
                .unwrap_or(entry.expected_size);

            let start_time = Instant::now();
            let app_ref = app.clone();
            let transfer_id = format!("psync-{}-{}", stream_id, index);
            let filename = entry.relative_path.clone();
            let mut last_emit_time_dl = Instant::now();
            let mut last_emit_pct_dl: u8 = 0;

            ftp.download_file_with_progress(
                &entry.remote_path,
                &entry.local_path,
                move |transferred| {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let speed = if elapsed > 0.0 { (transferred as f64 / elapsed) as u64 } else { 0 };
                    let pct = if file_size > 0 {
                        ((transferred as f64 / file_size as f64) * 100.0) as u8
                    } else { 0 };

                    // Throttle: emit every 150ms or 2% delta (matches standard transfer path)
                    let is_complete = transferred >= file_size && file_size > 0;
                    let time_ok = last_emit_time_dl.elapsed().as_millis() >= 150;
                    let pct_ok = pct.saturating_sub(last_emit_pct_dl) >= 2;
                    if time_ok || pct_ok || is_complete {
                        last_emit_time_dl = Instant::now();
                        last_emit_pct_dl = pct;
                        let _ = app_ref.emit("sync-parallel-progress", serde_json::json!({
                            "phase": "transfer_progress",
                            "stream_id": stream_id,
                            "transfer_id": transfer_id,
                            "relative_path": filename,
                            "direction": "download",
                            "transferred": transferred,
                            "total": file_size,
                            "percentage": pct,
                            "speed_bps": speed,
                            "index": index,
                            "total_files": total,
                        }));
                    }
                    true // continue
                },
            ).await.map_err(|e| format!("Download failed: {}", e))?;

            Ok("downloaded".to_string())
        }
        transfer_pool::TransferAction::Delete => {
            // Delete remote file
            ftp.remove(&entry.remote_path).await
                .map_err(|e| format!("Delete failed: {}", e))?;
            Ok("deleted".to_string())
        }
        transfer_pool::TransferAction::Mkdir => {
            // Mkdir handled at task level, skip here
            Ok("skipped".to_string())
        }
    };

    // Disconnect gracefully
    let _ = ftp.disconnect().await;

    result
}

// ─── End Phase 3A+ Commands ────────────────────────────────────────────

#[tauri::command]
fn get_default_retry_policy() -> RetryPolicy {
    RetryPolicy::default()
}

#[tauri::command]
fn verify_local_transfer(
    local_path: String,
    expected_size: u64,
    expected_mtime: Option<String>,
    expected_hash: Option<String>,
    policy: VerifyPolicy,
) -> VerifyResult {
    let mtime = expected_mtime.and_then(|s| {
        chrono::DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&chrono::Utc))
    });
    verify_local_file(&local_path, expected_size, mtime, &policy, expected_hash.as_deref())
}

#[tauri::command]
fn classify_transfer_error(raw_error: String, file_path: Option<String>) -> SyncErrorInfo {
    classify_sync_error(&raw_error, file_path.as_deref())
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

#[tauri::command]
async fn ai_list_models(
    provider_type: ai::AIProviderType,
    base_url: String,
    api_key: Option<String>,
) -> Result<Vec<String>, String> {
    ai::list_models(provider_type, base_url, api_key)
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
                let content = read_local_file(path.to_string(), Some(5))
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
                delete_local_file(app.clone(), state.clone(), path.to_string())
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
#[allow(clippy::too_many_arguments)]
async fn setup_aerocloud(
    cloud_name: String,
    local_folder: String,
    remote_folder: String,
    server_profile: String,
    sync_on_change: bool,
    sync_interval_secs: u64,
    protocol_type: Option<String>,
    connection_params: Option<serde_json::Value>,
) -> Result<CloudConfig, String> {
    let config = CloudConfig {
        enabled: true,
        cloud_name,
        local_folder: std::path::PathBuf::from(&local_folder),
        remote_folder: remote_folder.clone(),
        server_profile,
        sync_on_change,
        sync_interval_secs,
        protocol_type: protocol_type.unwrap_or_else(|| "ftp".to_string()),
        connection_params: connection_params.unwrap_or(serde_json::Value::Null),
        ..CloudConfig::default()
    };

    // Validate configuration
    cloud_config::validate_config(&config)?;

    // Ensure local cloud folder exists
    cloud_config::ensure_cloud_folder(&config)?;

    // Save configuration
    cloud_config::save_cloud_config(&config)?;

    info!("AeroCloud setup complete: protocol={}, local={}, remote={}",
        config.protocol_type, local_folder, remote_folder);

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

    // Save current working directory before sync — restore after to avoid
    // corrupting the UI session's CWD (the ftp_manager is shared with the UI)
    let saved_cwd = ftp_manager.current_path();

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

    let sync_result = cloud_service.perform_full_sync(&mut ftp_manager).await;

    // Restore CWD to prevent UI state corruption
    if let Err(e) = ftp_manager.change_dir(&saved_cwd).await {
        warn!("Failed to restore CWD after cloud sync: {}", e);
    }

    match sync_result {
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
            // Still try to restore CWD on error (already attempted above after sync)
            error!("Sync failed: {}", e);
            Err(format!("Sync failed: {}", e))
        }
    }
}
// ============ Background Sync & Tray Commands ============

use std::time::Duration;

// Global flag to control background sync
pub(crate) static BACKGROUND_SYNC_RUNNING: AtomicBool = AtomicBool::new(false);

/// Background sync worker — `tokio::select!` event loop
///
/// Listens for three trigger sources:
/// 1. **Scheduler timer**: fires based on `SyncSchedule` (interval + time window)
/// 2. **Filesystem watcher**: fires when files change in the local sync folder
/// 3. **Manual trigger**: fires when user clicks "Sync Now" via mpsc channel
///
/// Creates its own FTP connection per cycle to avoid conflicts with main UI.
async fn background_sync_worker(app: AppHandle) {
    info!("Background sync worker started (Phase 3A+ engine)");

    // --- Setup filesystem watcher (Dropbox-style real-time sync) ---
    let (watcher_tx, mut watcher_rx) = tokio::sync::mpsc::channel::<file_watcher::WatcherEvent>(64);
    let mut watcher: Option<file_watcher::FileWatcher> = None;

    {
        let config = cloud_config::load_cloud_config();
        if config.sync_on_change {
            let local_path = config.local_folder.clone();
            let mut fw = file_watcher::FileWatcher::new(watcher_tx.clone());
            match fw.start(&local_path, file_watcher::WatcherMode::Auto) {
                Ok(()) => {
                    info!("Filesystem watcher active on {}", local_path.display());
                    let _ = app.emit("cloud-watcher-status", serde_json::json!({
                        "active": true,
                        "path": local_path.to_string_lossy(),
                    }));
                    watcher = Some(fw);
                }
                Err(e) => {
                    warn!("Failed to start filesystem watcher: {}", e);
                }
            }
        }
    }

    // --- Main event loop ---
    let mut is_first_run = true;
    let mut last_sync_completed = tokio::time::Instant::now() - Duration::from_secs(120); // allow first sync immediately
    const WATCHER_COOLDOWN_SECS: u64 = 30; // min seconds between watcher-triggered syncs

    loop {
        // Check global stop flag
        if !BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
            info!("Background sync worker stopping (flag set to false)");
            break;
        }

        // Load fresh config and schedule each cycle
        let config = cloud_config::load_cloud_config();
        if !config.enabled {
            info!("AeroCloud disabled, stopping background sync");
            BACKGROUND_SYNC_RUNNING.store(false, Ordering::SeqCst);
            tray_badge::update_tray_badge(&app, tray_badge::TrayBadgeState::Default);
            let _ = app.emit("cloud-sync-status", serde_json::json!({
                "status": "disabled",
                "message": "AeroCloud is disabled"
            }));
            break;
        }

        // Determine trigger source for this cycle
        let trigger: transfer_pool::SyncTrigger = if is_first_run {
            is_first_run = false;
            // Only sync on startup if explicitly configured
            if config.sync_on_startup {
                transfer_pool::SyncTrigger::Manual
            } else {
                continue; // Skip first run, wait for normal interval
            }
        } else {
            // Load scheduler state
            let schedule = sync_scheduler::load_sync_schedule();

            // Emit schedule countdown to frontend
            if let Some(next_secs) = schedule.next_sync_in() {
                let _ = app.emit("cloud-sync-schedule", serde_json::json!({
                    "next_sync_in_secs": next_secs,
                    "enabled": schedule.enabled,
                    "paused": schedule.paused,
                    "in_time_window": schedule.is_in_time_window(),
                }));
            }

            // Compute sleep duration: min of scheduler interval and 5s poll
            let sleep_secs = if schedule.enabled && !schedule.paused {
                schedule.next_sync_in().unwrap_or(30).min(30)
            } else {
                config.sync_interval_secs.max(30)
            };

            // Wait using tokio::select! — first event wins
            tokio::select! {
                // Timer tick (scheduler interval or config interval)
                _ = tokio::time::sleep(Duration::from_secs(sleep_secs)) => {
                    // Check if schedule allows sync now
                    let schedule = sync_scheduler::load_sync_schedule();
                    if schedule.enabled && schedule.should_sync_now() {
                        transfer_pool::SyncTrigger::Scheduled
                    } else if !schedule.enabled {
                        // Fallback to legacy interval logic when scheduler is disabled
                        transfer_pool::SyncTrigger::Scheduled
                    } else {
                        continue; // Not time yet, loop again
                    }
                }
                // Filesystem watcher event
                Some(event) = watcher_rx.recv() => {
                    // Suppress watcher during active folder download/upload
                    if crate::provider_commands::TRANSFER_IN_PROGRESS.load(std::sync::atomic::Ordering::SeqCst) {
                        info!("Watcher trigger suppressed: folder transfer in progress");
                        while watcher_rx.try_recv().is_ok() {}
                        continue;
                    }
                    // Cooldown: skip watcher triggers too close to last sync
                    // (prevents loop: sync writes files → watcher detects → re-sync)
                    let elapsed = last_sync_completed.elapsed().as_secs();
                    if elapsed < WATCHER_COOLDOWN_SECS {
                        info!("Watcher trigger suppressed: {}s since last sync (cooldown {}s)",
                            elapsed, WATCHER_COOLDOWN_SECS);
                        // Drain any queued watcher events
                        while watcher_rx.try_recv().is_ok() {}
                        continue;
                    }
                    info!("Watcher trigger: {} paths changed", event.paths.len());
                    transfer_pool::SyncTrigger::FileChanged(event.paths)
                }
            }
        };

        // Check stop flag after wait
        if !BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
            break;
        }

        // --- Execute sync cycle ---
        let trigger_label = match &trigger {
            transfer_pool::SyncTrigger::Scheduled => "scheduled",
            transfer_pool::SyncTrigger::FileChanged(paths) => {
                info!("Watcher-triggered sync for {} changed paths", paths.len());
                "watcher"
            }
            transfer_pool::SyncTrigger::Manual => "manual",
            transfer_pool::SyncTrigger::Stop => break,
        };

        info!("Background sync: starting cycle (trigger: {})", trigger_label);

        // Update tray badge and emit status
        tray_badge::update_tray_badge(&app, tray_badge::TrayBadgeState::Syncing);
        let _ = app.emit("cloud-sync-status", serde_json::json!({
            "status": "syncing",
            "message": "Syncing...",
            "trigger": trigger_label,
        }));

        {
            let local_folder = std::path::Path::new(&config.local_folder);
            sync_badge::update_directory_state(local_folder, sync_badge::SyncBadgeState::Syncing).await;
        }

        match perform_background_sync(&config).await {
            Ok(result) => {
                info!("Background sync completed: {} uploaded, {} downloaded, {} errors",
                    result.uploaded, result.downloaded, result.errors.len());

                // Mark sync completed and drain watcher events generated by the sync itself
                last_sync_completed = tokio::time::Instant::now();
                let drained = {
                    let mut count = 0u32;
                    while watcher_rx.try_recv().is_ok() { count += 1; }
                    count
                };
                if drained > 0 {
                    info!("Drained {} watcher events generated during sync", drained);
                }

                {
                    let local_folder = std::path::Path::new(&config.local_folder);
                    sync_badge::update_directory_state(local_folder, sync_badge::SyncBadgeState::Synced).await;
                }

                tray_badge::update_tray_badge(&app, tray_badge::TrayBadgeState::Default);

                // Update scheduler last_sync timestamp
                let mut schedule = sync_scheduler::load_sync_schedule();
                schedule.last_sync = Some(chrono::Utc::now());
                let _ = sync_scheduler::save_sync_schedule(&schedule);

                let _ = app.emit("cloud-sync-status", serde_json::json!({
                    "status": "active",
                    "message": format!("Synced: ↑{} ↓{}", result.uploaded, result.downloaded)
                }));
                let _ = app.emit("cloud_sync_complete", &result);
            }
            Err(e) => {
                warn!("Background sync failed: {}", e);

                // Mark sync completed (even on error) and drain watcher events
                last_sync_completed = tokio::time::Instant::now();
                while watcher_rx.try_recv().is_ok() {}

                {
                    let local_folder = std::path::Path::new(&config.local_folder);
                    sync_badge::update_directory_state(local_folder, sync_badge::SyncBadgeState::Error).await;
                }

                tray_badge::update_tray_badge(&app, tray_badge::TrayBadgeState::Error);

                let _ = app.emit("cloud-sync-status", serde_json::json!({
                    "status": "error",
                    "message": format!("Sync failed: {}", e)
                }));

                // On error, wait before retrying
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }

    // --- Cleanup ---
    if let Some(mut fw) = watcher {
        fw.stop();
        info!("Filesystem watcher stopped");
    }
    let _ = app.emit("cloud-watcher-status", serde_json::json!({
        "active": false,
    }));

    info!("Background sync worker exited");
}

/// Perform a sync cycle with a dedicated provider connection.
/// Creates the appropriate provider based on config.protocol_type (FTP, SFTP, S3, Google Drive, etc.)
/// and uses the generic perform_full_sync_with_provider method.
async fn perform_background_sync(config: &cloud_config::CloudConfig) -> Result<cloud_service::SyncOperationResult, String> {
    info!("Background sync: creating {} provider for profile '{}'",
        config.protocol_type, config.server_profile);

    // Create and connect the provider via multi-protocol factory
    let mut provider = cloud_provider_factory::create_cloud_provider(config).await?;

    info!("Background sync: connected via {}", config.protocol_type);

    // Navigate to remote folder
    if provider.cd(&config.remote_folder).await.is_err() {
        // Try to create it
        let _ = provider.mkdir(&config.remote_folder).await;
        provider.cd(&config.remote_folder).await
            .map_err(|e| format!("Failed to navigate to remote folder: {}", e))?;
    }

    // Create cloud service and perform sync using the generic provider method
    let cloud_service = cloud_service::CloudService::new();
    cloud_service.init(config.clone()).await;

    let result = cloud_service.perform_full_sync_with_provider(provider.as_mut()).await?;

    // Disconnect
    let _ = provider.disconnect().await;

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

    // Start badge server for file manager integration (Nautilus/Nemo)
    if let Err(e) = sync_badge::start_badge_server(app.clone()).await {
        warn!("Badge server failed to start (non-fatal): {}", e);
    }

    // Register sync root so files in local folder show green badge
    let local_folder = std::path::PathBuf::from(&config.local_folder);
    sync_badge::register_sync_root(local_folder).await;

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

    // Stop badge server and clear sync roots
    sync_badge::stop_badge_server().await;
    sync_badge::clear_all_states().await;

    // Reset tray badge to default (no badge)
    tray_badge::update_tray_badge(&app, tray_badge::TrayBadgeState::Default);

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

#[tauri::command]
async fn update_tray_badge_cmd(app: AppHandle, state: String) -> Result<(), String> {
    let badge_state = tray_badge::TrayBadgeState::from_str(&state);
    tray_badge::update_tray_badge(&app, badge_state);
    Ok(())
}

/// Save server credentials for background sync use
#[tauri::command]
async fn save_server_credentials(
    profile_name: String,
    server: String,
    username: String,
    password: String,
) -> Result<(), String> {
    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "STORE_NOT_READY".to_string())?;

    let value = serde_json::json!({
        "server": server,
        "username": username,
        "password": password,
    });

    store.store(
        &format!("server_{}", profile_name),
        &value.to_string(),
    ).map_err(|e| format!("Failed to save credentials: {}", e))?;

    info!("Saved credentials for profile: {}", profile_name);
    Ok(())
}

// ============ Universal Credential Vault Commands ============

#[derive(Serialize)]
struct CredentialStoreStatus {
    master_mode: bool,
    is_locked: bool,
    vault_exists: bool,
    accounts_count: u32,
    timeout_seconds: u64,
}

#[tauri::command]
async fn init_credential_store() -> Result<String, String> {
    credential_store::CredentialStore::init()
        .map_err(|e| format!("Failed to initialize credential vault: {}", e))
}

#[tauri::command]
async fn get_credential_store_status(
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<CredentialStoreStatus, String> {
    let vault_exists = credential_store::CredentialStore::vault_exists();
    let master_mode = credential_store::CredentialStore::is_master_mode();
    let is_locked = state.is_locked();

    // Load persisted timeout on first status check (after restart)
    if master_mode && state.get_timeout() == 0 {
        let persisted = load_persisted_timeout();
        if persisted > 0 {
            state.set_timeout(persisted);
        }
    }

    let accounts_count = credential_store::CredentialStore::from_cache()
        .and_then(|store| store.list_accounts().ok())
        .map(|a| a.len() as u32)
        .unwrap_or(0);

    Ok(CredentialStoreStatus {
        master_mode,
        is_locked,
        vault_exists,
        accounts_count,
        timeout_seconds: state.get_timeout(),
    })
}

#[tauri::command]
async fn store_credential(account: String, password: String) -> Result<(), String> {
    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "STORE_NOT_READY".to_string())?;
    store.store(&account, &password)
        .map_err(|e| format!("Failed to store credential: {}", e))
}

#[tauri::command]
async fn get_credential(account: String) -> Result<String, String> {
    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "STORE_NOT_READY".to_string())?;
    store.get(&account)
        .map_err(|e| format!("Failed to get credential: {}", e))
}

#[tauri::command]
async fn delete_credential(account: String) -> Result<(), String> {
    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "STORE_NOT_READY".to_string())?;
    store.delete(&account)
        .map_err(|e| format!("Failed to delete credential: {}", e))
}

#[tauri::command]
async fn unlock_credential_store(
    password: String,
    totp_code: Option<String>,
    state: State<'_, master_password::MasterPasswordState>,
    totp_state: State<'_, totp::TotpState>,
) -> Result<(), String> {
    // Step 0: Check throttle (M69 — brute-force protection)
    if let Err(wait_secs) = state.check_throttle() {
        return Err(format!("THROTTLED:{}", wait_secs));
    }

    // Step 1: Verify master password and unlock vault
    match credential_store::CredentialStore::unlock_with_master(&password) {
        Ok(()) => {
            state.reset_throttle();
        }
        Err(e) => {
            state.record_failed_attempt();
            return Err(e.to_string());
        }
    }

    // Step 2: Check if TOTP is enabled by looking for stored secret
    let totp_secret = credential_store::CredentialStore::from_cache()
        .and_then(|store| store.get("totp_secret").ok());

    if let Some(secret) = totp_secret {
        if !secret.is_empty() {
            // TOTP is enabled — load secret into state and verify code
            totp::load_secret_internal(&totp_state, &secret)
                .map_err(|e| {
                    // Re-lock vault on failure (fail-closed)
                    credential_store::CredentialStore::lock();
                    state.set_locked(true);
                    format!("Failed to load TOTP secret: {}", e)
                })?;

            match totp_code {
                Some(ref code) if !code.is_empty() => {
                    let valid = totp::verify_internal(&totp_state, code)
                        .inspect_err(|_e| {
                            credential_store::CredentialStore::lock();
                            state.set_locked(true);
                        })?;
                    if !valid {
                        credential_store::CredentialStore::lock();
                        state.set_locked(true);
                        return Err("2FA_INVALID".to_string());
                    }
                }
                _ => {
                    // No TOTP code provided but 2FA is enabled
                    credential_store::CredentialStore::lock();
                    state.set_locked(true);
                    return Err("2FA_REQUIRED".to_string());
                }
            }
        }
    }

    state.set_locked(false);
    state.update_activity();
    Ok(())
}

#[tauri::command]
async fn lock_credential_store(
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<(), String> {
    credential_store::CredentialStore::lock();
    state.set_locked(true);
    Ok(())
}

#[tauri::command]
async fn enable_master_password(
    password: String,
    timeout_seconds: u32,
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<(), String> {
    credential_store::CredentialStore::enable_master_password(&password)
        .map_err(|e| e.to_string())?;
    let secs = timeout_seconds as u64;
    state.set_timeout(secs);
    persist_auto_lock_timeout(secs).ok(); // best-effort persist
    state.update_activity();
    Ok(())
}

#[tauri::command]
async fn disable_master_password(
    password: String,
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<(), String> {
    credential_store::CredentialStore::disable_master_password(&password)
        .map_err(|e| e.to_string())?;
    state.set_locked(false);
    state.set_timeout(0);
    Ok(())
}

#[tauri::command]
async fn change_master_password(
    old_password: String,
    new_password: String,
) -> Result<(), String> {
    credential_store::CredentialStore::change_master_password(&old_password, &new_password)
        .map_err(|e| e.to_string())
}

/// Persist auto-lock timeout to config file (not a secret, plain text)
fn persist_auto_lock_timeout(seconds: u64) -> Result<(), String> {
    let config_dir = dirs::config_dir()
        .ok_or("Cannot find config directory")?
        .join("aeroftp");
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    std::fs::write(config_dir.join("auto_lock_timeout"), seconds.to_string())
        .map_err(|e| e.to_string())
}

/// Load persisted auto-lock timeout from config file
fn load_persisted_timeout() -> u64 {
    dirs::config_dir()
        .map(|d| d.join("aeroftp").join("auto_lock_timeout"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

#[tauri::command]
async fn set_auto_lock_timeout(
    timeout_seconds: u32,
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<(), String> {
    let secs = timeout_seconds as u64;
    state.set_timeout(secs);
    persist_auto_lock_timeout(secs)?;
    Ok(())
}

#[tauri::command]
async fn app_master_password_status(
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<master_password::MasterPasswordStatus, String> {
    Ok(master_password::MasterPasswordStatus::new(&state))
}

#[tauri::command]
async fn app_master_password_update_activity(
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<(), String> {
    state.update_activity();
    Ok(())
}

#[tauri::command]
async fn app_master_password_check_timeout(
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<bool, String> {
    Ok(state.check_timeout())
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
        if let Some(store) = credential_store::CredentialStore::from_cache() {
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

    // Store credentials in secure store
    if let Some(store) = credential_store::CredentialStore::from_cache() {
        for server in &servers {
            if let Some(ref cred) = server.credential {
                let _ = store.store(&format!("server_{}", server.id), cred);
            }
        }
    }

    // H16 fix: Redact credentials before returning to renderer.
    // Only return non-sensitive fields + a boolean flag indicating stored credentials.
    let redacted_servers: Vec<serde_json::Value> = servers.iter().map(|s| {
        serde_json::json!({
            "id": s.id,
            "name": s.name,
            "host": s.host,
            "port": s.port,
            "username": s.username,
            "protocol": s.protocol,
            "initialPath": s.initial_path,
            "localInitialPath": s.local_initial_path,
            "color": s.color,
            "lastConnected": s.last_connected,
            "options": s.options,
            "providerId": s.provider_id,
            "hasStoredCredential": s.credential.is_some(),
        })
    }).collect();

    let result = serde_json::json!({
        "servers": redacted_servers,
        "metadata": metadata,
    });
    Ok(result)
}

#[tauri::command]
async fn read_export_metadata(file_path: String) -> Result<profile_export::ExportMetadata, String> {
    profile_export::read_metadata(std::path::Path::new(&file_path))
        .map_err(|e| e.to_string())
}

// ============ Full Keystore Export/Import ============

#[tauri::command]
async fn export_keystore(password: String, file_path: String) -> Result<keystore_export::KeystoreMetadata, String> {
    keystore_export::export_keystore(&password, std::path::Path::new(&file_path))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn import_keystore(
    password: String,
    file_path: String,
    merge_strategy: String,
) -> Result<keystore_export::KeystoreImportResult, String> {
    keystore_export::import_keystore(&password, std::path::Path::new(&file_path), &merge_strategy)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn read_keystore_metadata(file_path: String) -> Result<keystore_export::KeystoreMetadata, String> {
    keystore_export::read_keystore_metadata(std::path::Path::new(&file_path))
        .map_err(|e| e.to_string())
}

// ============ Public wrappers for AI tool access ============
// Cannot make #[tauri::command] functions pub (Tauri 2 macro conflict),
// so we expose thin wrappers that ai_tools.rs can call via crate::

pub async fn compress_files_core(paths: Vec<String>, output_path: String, password: Option<String>, compression_level: Option<i64>) -> Result<String, String> {
    compress_files(paths, output_path, password, compression_level).await
}

pub async fn extract_archive_core(archive_path: String, output_dir: String, create_subfolder: bool, password: Option<String>) -> Result<String, String> {
    extract_archive(archive_path, output_dir, create_subfolder, password).await
}

pub async fn compress_7z_core(paths: Vec<String>, output_path: String, password: Option<String>, compression_level: Option<i64>) -> Result<String, String> {
    compress_7z(paths, output_path, password, compression_level).await
}

pub async fn extract_7z_core(archive_path: String, output_dir: String, password: Option<String>, create_subfolder: bool) -> Result<String, String> {
    extract_7z(archive_path, output_dir, password, create_subfolder).await
}

pub async fn compress_tar_core(paths: Vec<String>, output_path: String, format: String, compression_level: Option<i64>) -> Result<String, String> {
    compress_tar(paths, output_path, format, compression_level).await
}

pub async fn extract_tar_core(archive_path: String, output_dir: String, create_subfolder: bool) -> Result<String, String> {
    extract_tar(archive_path, output_dir, create_subfolder).await
}

// ============ App Entry Point ============

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri::menu::{Menu, MenuItem, Submenu, PredefinedMenuItem};

    // Fix WebKitGTK rendering issues on Linux: disable DMA-BUF renderer
    // which causes canvas/WebGL artifacts in Monaco and xterm.js.
    // Must be set BEFORE any WebKit initialization.
    #[cfg(target_os = "linux")]
    {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    // Serve frontend via real HTTP server to fix WebKitGTK rendering issues.
    // In production, Tauri uses tauri:// custom protocol which breaks:
    // - Monaco Editor web workers (no syntax highlighting)
    // - xterm.js canvas renderer (no colors/cursor)
    // - iframe CSS rendering (no styles in HTML preview)
    // By serving via http://localhost, production behaves identically to dev mode.
    //
    // SECURITY NOTE (H26): This serves the frontend over unencrypted HTTP on localhost:14321.
    // This is a known design trade-off required by WebKitGTK on Linux — the tauri:// custom
    // protocol does not support web workers, canvas rendering, or iframe CSS in WebKitGTK.
    // Risk assessment:
    //   - Traffic is loopback-only (127.0.0.1), not exposed on network interfaces
    //   - Exploitation requires same-machine access (local privilege escalation prerequisite)
    //   - All sensitive data (credentials, tokens) flows through Tauri IPC commands, NOT HTTP
    //   - tauri-plugin-localhost binds exclusively to 127.0.0.1
    // This cannot be changed to HTTPS without a local TLS certificate infrastructure that
    // would add complexity with minimal security benefit for localhost-only traffic.
    let port: u16 = 14321;

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_localhost::Builder::new(port).build())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_log::Builder::default()
            .level(log::LevelFilter::Info)
            .build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // When a second instance is launched, show and focus the existing window
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .setup(move |app| {
            use tauri::tray::{TrayIconBuilder, MouseButton, MouseButtonState};

            // Ensure AppConfig directory exists with restricted permissions (0700)
            if let Ok(config_dir) = app.path().app_config_dir() {
                let _ = std::fs::create_dir_all(&config_dir);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700));
                }
            }

            // Initialize Chat History SQLite database
            match chat_history::init_db(app.handle()) {
                Ok(conn) => {
                    if let Err(e) = chat_history::migrate_from_json(&conn, app.handle()) {
                        log::warn!("Chat history migration failed: {e}");
                    }
                    app.manage(chat_history::ChatHistoryDb(std::sync::Mutex::new(conn)));
                }
                Err(e) => {
                    log::error!("Chat history DB init failed: {e}");
                    // Fallback: in-memory DB so commands don't panic
                    let conn = rusqlite::Connection::open_in_memory()
                        .expect("in-memory SQLite");
                    let _ = chat_history::init_db_schema(&conn);
                    app.manage(chat_history::ChatHistoryDb(std::sync::Mutex::new(conn)));
                }
            }

            // Initialize File Tags SQLite database
            match file_tags::init_db(app.handle()) {
                Ok(conn) => {
                    app.manage(file_tags::FileTagsDb(std::sync::Mutex::new(conn)));
                }
                Err(e) => {
                    log::error!("File tags DB init failed: {e}");
                    let conn = rusqlite::Connection::open_in_memory()
                        .expect("in-memory SQLite");
                    let _ = file_tags::init_db_schema(&conn);
                    app.manage(file_tags::FileTagsDb(std::sync::Mutex::new(conn)));
                }
            }

            // Start mount watcher — emits 'volumes-changed' events instead of 5s polling
            filesystem::start_mount_watcher(app.handle().clone());

            // Navigate main window from tauri:// to http://localhost to fix
            // WebKitGTK rendering issues with Monaco, xterm.js, and iframes.
            // Only in production — in dev mode, Tauri uses devUrl (Vite on :5173).
            #[cfg(not(dev))]
            if let Some(window) = app.get_webview_window("main") {
                let url = url::Url::parse(&format!("http://localhost:{}", port))
                    .expect("valid localhost URL");
                let _ = window.navigate(url);
            }

            let accel = |shortcut: &'static str| -> Option<&'static str> {
                #[cfg(target_os = "linux")]
                {
                    let _ = shortcut;
                    None
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Some(shortcut)
                }
            };

            // Create menu items
            let quit = MenuItem::with_id(app, "quit", "Quit AeroFTP", true, accel("CmdOrCtrl+Q"))?;
            let about = MenuItem::with_id(app, "about", "About AeroFTP", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "Settings...", true, accel("CmdOrCtrl+,"))?;
            let refresh = MenuItem::with_id(app, "refresh", "Refresh", true, accel("CmdOrCtrl+R"))?;
            let shortcuts = MenuItem::with_id(app, "shortcuts", "Keyboard Shortcuts", true, accel("F1"))?;
            let support = MenuItem::with_id(app, "support", "Support Development ❤️", true, None::<&str>)?;
            
            // File menu
            let file_menu = Submenu::with_items(
                app,
                "File",
                true,
                &[
                    &MenuItem::with_id(app, "new_folder", "New Folder", true, accel("CmdOrCtrl+N"))?,
                    &PredefinedMenuItem::separator(app)?,
                    &settings,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "toggle_debug_mode", "Debug Mode", true, accel("CmdOrCtrl+Shift+F12"))?,
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
                    &PredefinedMenuItem::undo(app, None)?,
                    &PredefinedMenuItem::redo(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "rename", "Rename", true, accel("F2"))?,
                    &MenuItem::with_id(app, "delete", "Delete", true, accel("Delete"))?,
                ],
            )?;
            
            // View menu
            let devtools_submenu = Submenu::with_items(
                app,
                "DevTools",
                true,
                &[
                    &MenuItem::with_id(app, "toggle_devtools", "Toggle DevTools", true, accel("CmdOrCtrl+Shift+D"))?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "toggle_editor", "Toggle Editor", true, accel("CmdOrCtrl+1"))?,
                    &MenuItem::with_id(app, "toggle_terminal", "Toggle Terminal", true, accel("CmdOrCtrl+2"))?,
                    &MenuItem::with_id(app, "toggle_agent", "Toggle Agent", true, accel("CmdOrCtrl+3"))?,
                ],
            )?;
            
            let view_menu = Submenu::with_items(
                app,
                "View",
                true,
                &[
                    &refresh,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "toggle_theme", "Toggle Theme", true, accel("CmdOrCtrl+T"))?,
                    &PredefinedMenuItem::separator(app)?,
                    &devtools_submenu,
                ],
            )?;
            
            // Help menu
            let check_update = MenuItem::with_id(app, "check_update", "Check for Updates", true, None::<&str>)?;

            let help_menu = Submenu::with_items(
                app,
                "Help",
                true,
                &[
                    &check_update,
                    &PredefinedMenuItem::separator(app)?,
                    &shortcuts,
                    &PredefinedMenuItem::separator(app)?,
                    &support,
                    &PredefinedMenuItem::separator(app)?,
                    &about,
                ],
            )?;
            
            // === Splash Screen ===
            // Create splash BEFORE setting the global app menu so it never inherits it.
            // The main window is hidden (visible: false in tauri.conf.json) until
            // the frontend signals readiness via the `app_ready` command.
            let splash_url = {
                #[cfg(dev)]
                { WebviewUrl::External(url::Url::parse("http://localhost:5173/splash.html").unwrap()) }
                #[cfg(not(dev))]
                { WebviewUrl::External(url::Url::parse(&format!("http://localhost:{}/splash.html", port)).unwrap()) }
            };

            let _splash = WebviewWindowBuilder::new(app, "splashscreen", splash_url)
                .title("AeroFTP")
                .inner_size(420.0, 340.0)
                .resizable(false)
                .decorations(false)
                .center()
                .build()?;

            SPLASH_CREATED_AT.get_or_init(std::time::Instant::now);
            info!("Splash screen created");

            // Build menu but do NOT set it globally yet — GTK applies global menus
            // to ALL windows instantly, causing a menu flash on the splash screen.
            // The menu will be set in app_ready() after the splash is closed.
            let menu = Menu::with_items(app, &[&file_menu, &edit_menu, &view_menu, &help_menu])?;
            app.manage(std::sync::Mutex::new(Some(menu)));

            // Safety timeout: if frontend doesn't signal app_ready within 10 seconds,
            // force-close splash, set deferred menu, and show main window.
            // Skipped entirely if app_ready already ran (prevents window re-show).
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(10));
                if APP_READY_DONE.load(Ordering::SeqCst) {
                    return; // app_ready already handled everything
                }
                warn!("Splash screen safety timeout reached, force-closing");
                if let Some(splash) = app_handle.get_webview_window("splashscreen") {
                    let _ = splash.close();
                }
                // Set deferred menu
                if let Some(deferred) = app_handle.try_state::<std::sync::Mutex<Option<tauri::menu::Menu<tauri::Wry>>>>() {
                    if let Ok(mut guard) = deferred.lock() {
                        if let Some(menu) = guard.take() {
                            let _ = app_handle.set_menu(menu);
                        }
                    }
                }
                if let Some(main) = app_handle.get_webview_window("main") {
                    let _ = main.remove_menu();
                    let _ = main.show();
                    let _ = main.set_focus();
                }
            });

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
            
            // Build tray icon using white monochrome icon (standard for system tray)
            let tray_png = image::load_from_memory(
                include_bytes!("../../icons/AeroFTP_simbol_white_120x120.png")
            ).expect("Failed to decode tray icon");
            let tray_rgba = tray_png.to_rgba8();
            let (w, h) = tray_rgba.dimensions();
            let icon = tauri::image::Image::new_owned(tray_rgba.into_raw(), w, h);
            
            let _tray = TrayIconBuilder::with_id("main")
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
            // Only handle close events for the main window
            if window.label() != "main" {
                return;
            }
            // Hide window instead of closing when AeroCloud is enabled
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let cloud_config = cloud_config::load_cloud_config();
                if cloud_config.enabled {
                    info!("Window close requested, hiding to tray (AeroCloud enabled)");
                    let _ = window.hide();
                    api.prevent_close();
                } else {
                    // P1-5: Cleanup Cloud Filter root registrations on app exit
                    #[cfg(windows)]
                    {
                        if let Err(e) = crate::cloud_filter_badge::cleanup_all_roots() {
                            warn!("Cloud Filter cleanup on exit: {}", e);
                        }
                    }
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
    // Master Password state for app-level security
    let builder = builder.manage(master_password::MasterPasswordState::new());
    let builder = builder.manage(totp::TotpState::default());

    builder
        .invoke_handler(tauri::generate_handler![
            app_ready,
            copy_to_clipboard,
            resolve_hostname,
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
            reset_cancel_flag,
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
            copy_local_file,
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
            detect_server_favicon,
            detect_provider_favicon,
            save_local_file,
            save_remote_file,
            toggle_menu_bar,
            rebuild_menu,
            compare_directories,
            get_compare_options_default,
            load_sync_index_cmd,
            save_sync_index_cmd,
            load_sync_journal_cmd,
            save_sync_journal_cmd,
            delete_sync_journal_cmd,
            list_sync_journals_cmd,
            cleanup_old_journals_cmd,
            clear_all_journals_cmd,
            load_sync_profiles_cmd,
            save_sync_profile_cmd,
            delete_sync_profile_cmd,
            // Phase 3A+: Parallel sync, scan, scheduler, watcher
            parallel_sync_execute,
            get_parallel_scan_files,
            get_sync_schedule_cmd,
            save_sync_schedule_cmd,
            get_watcher_status_cmd,
            get_transfer_optimization_hints,
            get_multi_path_config,
            save_multi_path_config_cmd,
            add_path_pair,
            remove_path_pair,
            export_sync_template_cmd,
            import_sync_template_cmd,
            create_sync_snapshot_cmd,
            list_sync_snapshots_cmd,
            delete_sync_snapshot_cmd,
            delta_sync_analyze,
            sync_canary_run,
            sync_canary_approve,
            sign_sync_journal,
            verify_journal_signature,
            get_default_retry_policy,
            verify_local_transfer,
            classify_transfer_error,
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
            update_tray_badge_cmd,
            save_server_credentials,
            // Universal Credential Vault
            init_credential_store,
            get_credential_store_status,
            store_credential,
            get_credential,
            delete_credential,
            unlock_credential_store,
            lock_credential_store,
            enable_master_password,
            disable_master_password,
            change_master_password,
            set_auto_lock_timeout,
            app_master_password_status,
            app_master_password_update_activity,
            app_master_password_check_timeout,
            // Profile Export/Import
            export_server_profiles,
            import_server_profiles,
            read_export_metadata,
            // Full Keystore Export/Import
            export_keystore,
            import_keystore,
            read_keystore_metadata,
            // Debug & dependencies commands
            get_dependencies,
            check_crate_versions,
            get_system_info,
            // Updater commands
            check_update,
            log_update_detection,
            download_update,
            install_appimage_update,
            install_deb_update,
            install_rpm_update,
            // AI commands
            ai_chat,
            ai_test_provider,
            ai_list_models,
            ai_execute_tool,
            ai_tools::validate_tool_args,
            ai_tools::execute_ai_tool,
            ai_tools::shell_execute,
            ai_tools::clipboard_read_image,
            // Context Intelligence commands
            context_intelligence::detect_project_context,
            context_intelligence::scan_file_imports,
            context_intelligence::get_git_context,
            context_intelligence::read_agent_memory,
            context_intelligence::write_agent_memory,
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
            aerovault_v2::vault_v2_create_directory,
            aerovault_v2::vault_v2_delete_entries,
            aerovault_v2::vault_v2_add_files_to_dir,
            aerovault_v2::vault_v2_compact,
            aerovault_v2::vault_v2_sync_compare,
            aerovault_v2::vault_v2_sync_apply,
            // Remote Vault — open .aerovault on remote servers
            vault_remote::vault_v2_download_remote,
            vault_remote::vault_v2_upload_remote,
            vault_remote::vault_v2_cleanup_temp,
            // Cryptomator vault support
            cryptomator::cryptomator_unlock,
            cryptomator::cryptomator_lock,
            cryptomator::cryptomator_list,
            cryptomator::cryptomator_decrypt_file,
            cryptomator::cryptomator_encrypt_file,
            cryptomator::cryptomator_create,
            ai_stream::ai_chat_stream,
            ai::ollama_pull_model,
            ai::gemini_create_cache,
            ai::ollama_list_running,
            ai::kimi_create_cache,
            ai::kimi_upload_file,
            ai::deepseek_fim_complete,
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
            provider_commands::provider_server_copy,
            provider_commands::provider_supports_server_copy,
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
            // 4shared OAuth 1.0 commands
            provider_commands::fourshared_start_auth,
            provider_commands::fourshared_complete_auth,
            provider_commands::fourshared_full_auth,
            provider_commands::fourshared_connect,
            provider_commands::fourshared_has_tokens,
            provider_commands::fourshared_logout,
            provider_commands::zoho_list_trash,
            provider_commands::zoho_permanent_delete,
            provider_commands::zoho_restore_from_trash,
            provider_commands::jottacloud_move_to_trash,
            provider_commands::jottacloud_list_trash,
            provider_commands::jottacloud_restore_from_trash,
            provider_commands::jottacloud_permanent_delete,
            provider_commands::mega_move_to_trash,
            provider_commands::mega_list_trash,
            provider_commands::mega_restore_from_trash,
            provider_commands::mega_permanent_delete,
            provider_commands::google_drive_trash_file,
            provider_commands::google_drive_list_trash,
            provider_commands::google_drive_restore_from_trash,
            provider_commands::google_drive_permanent_delete,
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
            ssh_shell_close,
            // Host key verification (TOFU UX)
            sftp_check_host_key,
            sftp_accept_host_key,
            sftp_remove_host_key,
            // Plugin system
            plugins::list_plugins,
            plugins::execute_plugin_tool,
            plugins::install_plugin,
            plugins::remove_plugin,
            plugins::trigger_plugin_hooks,
            // Plugin registry
            plugin_registry::fetch_plugin_registry,
            plugin_registry::install_plugin_from_registry,
            // Filesystem (Places Sidebar + AeroFile)
            filesystem::get_user_directories,
            filesystem::list_mounted_volumes,
            filesystem::list_subdirectories,
            filesystem::eject_volume,
            filesystem::list_unmounted_partitions,
            filesystem::mount_partition,
            filesystem::get_file_properties,
            filesystem::calculate_folder_size,
            filesystem::delete_to_trash,
            filesystem::list_trash_items,
            filesystem::restore_trash_item,
            filesystem::empty_trash,
            filesystem::find_duplicate_files,
            filesystem::scan_disk_usage,
            filesystem::volumes_changed,
            // Mission Green Badge - File sync status tracking
            sync_badge::start_badge_server_cmd,
            sync_badge::stop_badge_server_cmd,
            sync_badge::set_file_badge,
            sync_badge::clear_file_badge,
            sync_badge::get_badge_status,
            sync_badge::install_shell_extension_cmd,
            sync_badge::uninstall_shell_extension_cmd,
            sync_badge::restart_file_manager_cmd,
            // Security Toolkit — Cyber Tools
            cyber_tools::hash_text,
            cyber_tools::hash_file,
            cyber_tools::compare_hashes,
            cyber_tools::crypto_encrypt_text,
            cyber_tools::crypto_decrypt_text,
            cyber_tools::generate_password,
            cyber_tools::generate_passphrase,
            cyber_tools::calculate_entropy,
            // TOTP 2FA
            totp::totp_setup_start,
            totp::totp_setup_verify,
            totp::totp_verify,
            totp::totp_status,
            totp::totp_enable,
            totp::totp_disable,
            totp::totp_load_secret,
            // Chat History SQLite
            chat_history::chat_history_init,
            chat_history::chat_history_list_sessions,
            chat_history::chat_history_get_session,
            chat_history::chat_history_create_session,
            chat_history::chat_history_save_message,
            chat_history::chat_history_update_session_title,
            chat_history::chat_history_delete_session,
            chat_history::chat_history_delete_sessions_bulk,
            chat_history::chat_history_clear_all,
            chat_history::chat_history_search,
            chat_history::chat_history_cleanup,
            chat_history::chat_history_stats,
            chat_history::chat_history_export_session,
            chat_history::chat_history_import,
            chat_history::chat_history_create_branch,
            chat_history::chat_history_switch_branch,
            chat_history::chat_history_delete_branch,
            chat_history::chat_history_save_branch_message,
            // File Tags SQLite
            file_tags::file_tags_list_labels,
            file_tags::file_tags_create_label,
            file_tags::file_tags_update_label,
            file_tags::file_tags_delete_label,
            file_tags::file_tags_set_tags,
            file_tags::file_tags_remove_tag,
            file_tags::file_tags_get_tags_for_files,
            file_tags::file_tags_get_files_by_label,
            file_tags::file_tags_get_label_counts,
            // AeroImage
            image_edit::process_image,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}