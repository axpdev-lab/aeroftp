//! AeroFTP Mission Green Badge - File sync status tracking and Nautilus extension integration
//!
//! This module provides:
//! - Unix socket server for file manager extensions (Nextcloud-compatible protocol)
//! - Per-file sync state tracking with LRU eviction
//! - Shell extension installation for Nautilus/Nemo
//! - GIO emblem fallback support

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, RwLock};
use tracing::{error, info, warn};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(any(unix, windows))]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(any(unix, windows))]
use tokio::sync::broadcast;

/// Maximum line length for socket protocol reads (prevents memory exhaustion DoS)
#[cfg(any(unix, windows))]
const MAX_LINE_LENGTH: usize = 8192;

// ============================================================================
// Types
// ============================================================================

/// Sync badge states for individual files
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncBadgeState {
    Synced,   // OK - file is synced
    Syncing,  // SYNC - sync in progress
    Error,    // ERROR - sync error
    Ignored,  // IGNORE - excluded from sync
    Conflict, // CONFLICT - merge conflict
    New,      // NEW - new file pending upload
}

impl SyncBadgeState {
    /// Convert to Nextcloud protocol status string
    pub fn to_status_str(self) -> &'static str {
        match self {
            SyncBadgeState::Synced => "OK",
            SyncBadgeState::Syncing => "SYNC",
            SyncBadgeState::Error => "ERROR",
            SyncBadgeState::Ignored => "IGNORE",
            SyncBadgeState::Conflict => "CONFLICT",
            SyncBadgeState::New => "NEW",
        }
    }

    /// Convert to GIO emblem name
    pub fn to_emblem_name(self) -> &'static str {
        match self {
            SyncBadgeState::Synced => "emblem-aerocloud-synced",
            SyncBadgeState::Syncing => "emblem-aerocloud-syncing",
            SyncBadgeState::Error => "emblem-aerocloud-error",
            SyncBadgeState::Ignored => "emblem-aerocloud-ignored",
            SyncBadgeState::Conflict => "emblem-aerocloud-conflict",
            SyncBadgeState::New => "emblem-aerocloud-new",
        }
    }
}

/// File sync state tracker with LRU eviction
struct SyncStateTracker {
    /// Per-file sync states
    states: HashMap<PathBuf, SyncBadgeState>,
    /// LRU access order (front = most recent)
    access_order: VecDeque<PathBuf>,
    /// Registered sync root directories
    sync_roots: Vec<PathBuf>,
    /// Max tracked files before LRU eviction
    max_entries: usize,
}

impl SyncStateTracker {
    fn new() -> Self {
        Self {
            states: HashMap::new(),
            access_order: VecDeque::new(),
            sync_roots: Vec::new(),
            max_entries: 100_000,
        }
    }

    /// Update state for a file and touch LRU
    fn set_state(&mut self, path: PathBuf, state: SyncBadgeState) {
        self.access_order.retain(|p| p != &path);
        self.access_order.push_front(path.clone());
        self.states.insert(path, state);

        // Evict LRU if over limit
        while self.states.len() > self.max_entries {
            if let Some(oldest) = self.access_order.pop_back() {
                self.states.remove(&oldest);
            }
        }
    }

    /// Get state for a file (touches LRU)
    fn get_state(&mut self, path: &Path) -> Option<SyncBadgeState> {
        if let Some(&state) = self.states.get(path) {
            let path_buf = path.to_path_buf();
            self.access_order.retain(|p| p != &path_buf);
            self.access_order.push_front(path_buf);
            Some(state)
        } else {
            None
        }
    }

    /// Remove state for a file
    fn remove_state(&mut self, path: &Path) {
        self.states.remove(path);
        self.access_order.retain(|p| p != path);
    }

    /// Check if path is inside a sync root
    fn is_in_sync_root(&self, path: &Path) -> bool {
        self.sync_roots.iter().any(|root| path.starts_with(root))
    }

    /// Register a sync root
    fn add_sync_root(&mut self, root: PathBuf) {
        if !self.sync_roots.contains(&root) {
            self.sync_roots.push(root);
        }
    }

    /// Clear all tracked states
    fn clear_all(&mut self) {
        self.states.clear();
        self.access_order.clear();
    }
}

// ============================================================================
// Global State
// ============================================================================

static BADGE_TRACKER: LazyLock<Arc<RwLock<SyncStateTracker>>> =
    LazyLock::new(|| Arc::new(RwLock::new(SyncStateTracker::new())));

static BADGE_SERVER_RUNNING: AtomicBool = AtomicBool::new(false);

#[cfg(any(unix, windows))]
static SHUTDOWN_TX: LazyLock<Arc<RwLock<Option<broadcast::Sender<()>>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(None)));

// ============================================================================
// Path Validation (audit fix: require absolute, block control chars)
// ============================================================================

/// Validate path for security: null bytes, length, traversal, absolute, control chars
fn validate_path(path: &str) -> Result<PathBuf, String> {
    if path.contains('\0') {
        return Err("Path contains null bytes".to_string());
    }

    // Block newlines and carriage returns (protocol injection prevention)
    if path.contains('\n') || path.contains('\r') {
        return Err("Path contains control characters".to_string());
    }

    if path.len() > 4096 {
        return Err("Path too long".to_string());
    }

    // Block UNC paths on Windows (network traversal prevention — audit fix SBA-005)
    #[cfg(windows)]
    if path.starts_with(r"\\") {
        return Err("UNC network paths not allowed".to_string());
    }

    let path_buf = PathBuf::from(path);

    // Require absolute path (audit fix GB-003)
    if !path_buf.is_absolute() {
        return Err("Path must be absolute".to_string());
    }

    for component in path_buf.components() {
        if component == Component::ParentDir {
            return Err("Path contains '..' components".to_string());
        }
    }

    Ok(path_buf)
}

// ============================================================================
// Bounded Line Reader (audit fix GB-001: prevent memory exhaustion)
// ============================================================================

/// Read a line from any AsyncBufRead with a maximum length limit.
/// Uses fill_buf() to enforce the limit BEFORE growing the output buffer,
/// preventing memory exhaustion from malicious clients sending huge lines.
/// The intermediate `to_vec()` is bounded by BufReader's internal buffer (~8KB).
#[cfg(any(unix, windows))]
async fn read_line_limited_generic<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut String,
) -> Result<usize, String> {
    buf.clear();
    let mut total = 0usize;

    loop {
        // Peek at buffered data and copy relevant chunk (releases borrow for consume)
        let chunk = {
            let available = reader
                .fill_buf()
                .await
                .map_err(|e| format!("Read error: {}", e))?;

            if available.is_empty() {
                return Ok(total); // EOF
            }

            // Copy up to newline (bounded by BufReader internal buffer, typically 8KB)
            if let Some(pos) = available.iter().position(|&b| b == b'\n') {
                available[..=pos].to_vec()
            } else {
                available.to_vec()
            }
        };
        // `available` dropped here — reader borrow released

        let chunk_len = chunk.len();
        let found_newline = chunk.last() == Some(&b'\n');

        // Check limit BEFORE appending to output buffer
        if total + chunk_len > MAX_LINE_LENGTH {
            reader.consume(chunk_len);
            return Err(format!(
                "Line exceeds maximum length of {} bytes",
                MAX_LINE_LENGTH
            ));
        }

        let s = std::str::from_utf8(&chunk)
            .map_err(|e| format!("UTF-8 error: {}", e))?;
        buf.push_str(s);
        total += chunk_len;
        reader.consume(chunk_len);

        if found_newline {
            return Ok(total);
        }
    }
}

/// Handle a single protocol line from any AsyncBufRead + AsyncWrite pair.
#[cfg(any(unix, windows))]
async fn handle_protocol_line_generic<R, W>(
    line: &str,
    reader: &mut R,
    writer: &mut W,
) -> Result<(), String>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    if line == "VERSION:" {
        writer
            .write_all(b"VERSION:1.0:AeroCloud\n")
            .await
            .map_err(|e| format!("Write error: {}", e))?;
        return Ok(());
    }

    if line == "RETRIEVE_FILE_STATUS" {
        let mut path_line = String::new();
        read_line_limited_generic(reader, &mut path_line).await?;

        let path_line = path_line.trim();
        if !path_line.starts_with("path\t") {
            return Err("Expected 'path\\t' line".to_string());
        }

        let path_str = &path_line[5..];
        let path = validate_path(path_str)?;

        let mut done_line = String::new();
        read_line_limited_generic(reader, &mut done_line).await?;

        if done_line.trim() != "done" {
            return Err("Expected 'done' line".to_string());
        }

        let status = {
            let mut tracker = BADGE_TRACKER.write().unwrap_or_else(|p| p.into_inner());
            if let Some(state) = tracker.get_state(&path) {
                state.to_status_str().to_string()
            } else if tracker.is_in_sync_root(&path) {
                "OK".to_string()
            } else {
                "NOP".to_string()
            }
        };

        let safe_path = path_str.replace(['\n', '\r'], "");
        let response = format!("STATUS:{}:{}\ndone\n", status, safe_path);
        writer
            .write_all(response.as_bytes())
            .await
            .map_err(|e| format!("Write error: {}", e))?;

        return Ok(());
    }

    Ok(())
}

/// Idle timeout for connected clients (audit fix SBA-004)
#[cfg(any(unix, windows))]
const CLIENT_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Handle a client connection using generic reader/writer with rate limiting, idle timeout and shutdown.
#[cfg(any(unix, windows))]
async fn handle_client_generic<R, W>(
    reader: R,
    mut writer: W,
    conn_id: u32,
    mut shutdown_rx: broadcast::Receiver<()>,
) where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    info!("Client {} connected", conn_id);

    let mut buf_reader = BufReader::new(reader);
    let mut line_buf = String::new();

    let mut window_start = std::time::Instant::now();
    let mut window_count = 0u32;

    loop {
        line_buf.clear();

        tokio::select! {
            result = tokio::time::timeout(CLIENT_IDLE_TIMEOUT, read_line_limited_generic(&mut buf_reader, &mut line_buf)) => {
                match result {
                    Ok(Ok(0)) => break, // EOF
                    Ok(Ok(_)) => {
                        let elapsed = window_start.elapsed();
                        if elapsed.as_secs() >= 1 {
                            window_start = std::time::Instant::now();
                            window_count = 0;
                        }
                        window_count += 1;
                        if window_count > 100 {
                            warn!("Client {} exceeded rate limit (100/s), closing", conn_id);
                            break;
                        }

                        let line = line_buf.trim();
                        if line.is_empty() {
                            continue;
                        }

                        if let Err(e) = handle_protocol_line_generic(line, &mut buf_reader, &mut writer).await {
                            error!("Client {} protocol error: {}", conn_id, e);
                            break;
                        }
                    }
                    Ok(Err(e)) => {
                        error!("Client {} error: {}", conn_id, e);
                        break;
                    }
                    Err(_) => {
                        info!("Client {} idle timeout ({}s), closing", conn_id, CLIENT_IDLE_TIMEOUT.as_secs());
                        break;
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                info!("Client {} received shutdown signal", conn_id);
                break;
            }
        }
    }

    info!("Client {} disconnected", conn_id);
}

// ============================================================================
// Public API Functions
// ============================================================================

/// Initialize and start the badge server
#[cfg(unix)]
pub async fn start_badge_server(_app_handle: tauri::AppHandle) -> Result<(), String> {
    if BADGE_SERVER_RUNNING.load(Ordering::Acquire) {
        return Err("Badge server already running".to_string());
    }

    let socket_path = get_socket_path()?;
    info!("Starting badge server at {:?}", socket_path);

    // Create socket directory
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create socket dir: {}", e))?;

        // Set directory permissions to 0700
        let metadata = std::fs::metadata(parent)
            .map_err(|e| format!("Failed to get dir metadata: {}", e))?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(parent, permissions)
            .map_err(|e| format!("Failed to set dir permissions: {}", e))?;
    }

    // Remove existing socket file unconditionally (audit fix GB-009: avoid TOCTOU)
    let _ = std::fs::remove_file(&socket_path);

    // Create Unix socket listener
    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| format!("Failed to bind socket: {}", e))?;

    // Set socket permissions to 0600
    let metadata = std::fs::metadata(&socket_path)
        .map_err(|e| format!("Failed to get socket metadata: {}", e))?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&socket_path, permissions)
        .map_err(|e| format!("Failed to set socket permissions: {}", e))?;

    // Create shutdown channel
    let (shutdown_tx, _) = broadcast::channel(1);
    *SHUTDOWN_TX.write().unwrap_or_else(|p| p.into_inner()) = Some(shutdown_tx.clone());

    BADGE_SERVER_RUNNING.store(true, Ordering::Release);

    // Concurrent connection limiter (audit fix GB-002: Semaphore instead of counter)
    let semaphore = Arc::new(tokio::sync::Semaphore::new(10));

    // Spawn accept loop
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown_tx.subscribe();
        let mut conn_id = 0u32;
        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            let permit = match semaphore.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    warn!("Max concurrent connections (10) reached, rejecting");
                                    drop(stream);
                                    continue;
                                }
                            };
                            conn_id = conn_id.wrapping_add(1);
                            let client_shutdown_rx = shutdown_tx.subscribe();
                            let id = conn_id;
                            tokio::spawn(async move {
                                handle_client(stream, id, client_shutdown_rx).await;
                                drop(permit); // Release semaphore when client disconnects
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept connection: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Shutdown signal received, stopping accept loop");
                    break;
                }
            }
        }
    });

    info!("Badge server started successfully");
    Ok(())
}

// ============================================================================
// Windows Named Pipe Server Implementation
// ============================================================================

#[cfg(windows)]
const PIPE_NAME: &str = r"\\.\pipe\aerocloud-sync";

#[cfg(windows)]
pub async fn start_badge_server(_app_handle: tauri::AppHandle) -> Result<(), String> {
    use tokio::net::windows::named_pipe::ServerOptions;

    if BADGE_SERVER_RUNNING.load(Ordering::Acquire) {
        return Err("Badge server already running".to_string());
    }

    info!("Starting Named Pipe badge server at {}", PIPE_NAME);

    // Test-create the first pipe instance to fail fast
    let first_pipe = ServerOptions::new()
        .first_pipe_instance(true) // Prevent pipe squatting (security)
        .reject_remote_clients(true) // Block network access (local-only IPC)
        .create(PIPE_NAME)
        .map_err(|e| format!("Failed to create named pipe: {}", e))?;

    // Create shutdown channel
    let (shutdown_tx, _) = broadcast::channel(1);
    *SHUTDOWN_TX.write().unwrap_or_else(|p| p.into_inner()) = Some(shutdown_tx.clone());

    BADGE_SERVER_RUNNING.store(true, Ordering::Release);

    let semaphore = Arc::new(tokio::sync::Semaphore::new(10));

    tokio::spawn(async move {
        let mut shutdown_rx = shutdown_tx.subscribe();
        let mut conn_id = 0u32;
        let mut server = first_pipe;

        loop {
            tokio::select! {
                result = server.connect() => {
                    match result {
                        Ok(()) => {
                            let permit = match semaphore.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    warn!("Max concurrent pipe connections (10) reached, rejecting");
                                    // Create new pipe for next client, drop current
                                    match ServerOptions::new().reject_remote_clients(true).create(PIPE_NAME) {
                                        Ok(new_pipe) => server = new_pipe,
                                        Err(e) => {
                                            error!("Failed to create replacement pipe: {}", e);
                                            break;
                                        }
                                    }
                                    continue;
                                }
                            };

                            conn_id = conn_id.wrapping_add(1);
                            let client_shutdown_rx = shutdown_tx.subscribe();
                            let id = conn_id;

                            // Split the connected pipe for the client
                            let (reader, writer) = tokio::io::split(server);

                            tokio::spawn(async move {
                                handle_client_generic(reader, writer, id, client_shutdown_rx).await;
                                drop(permit);
                            });

                            // Create a new pipe instance for the next client
                            match ServerOptions::new().create(PIPE_NAME) {
                                Ok(new_pipe) => server = new_pipe,
                                Err(e) => {
                                    error!("Failed to create next pipe instance: {}", e);
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            error!("Named pipe accept error: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Shutdown signal received, stopping pipe accept loop");
                    break;
                }
            }
        }
    });

    info!("Named Pipe badge server started successfully");
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub async fn start_badge_server(_app_handle: tauri::AppHandle) -> Result<(), String> {
    Err("Badge server not supported on this platform".to_string())
}

/// Stop the badge server and clean up
#[cfg(unix)]
pub async fn stop_badge_server() {
    if !BADGE_SERVER_RUNNING.load(Ordering::Acquire) {
        return;
    }

    info!("Stopping badge server");

    // Send shutdown signal (audit fix GB-005: handle poisoned lock)
    if let Some(tx) = SHUTDOWN_TX.write().unwrap_or_else(|p| p.into_inner()).take() {
        let _ = tx.send(());
    }

    // Remove socket file
    if let Ok(socket_path) = get_socket_path() {
        let _ = std::fs::remove_file(socket_path);
    }

    BADGE_SERVER_RUNNING.store(false, Ordering::Release);
    info!("Badge server stopped");
}

#[cfg(windows)]
pub async fn stop_badge_server() {
    if !BADGE_SERVER_RUNNING.load(Ordering::Acquire) {
        return;
    }

    info!("Stopping Named Pipe badge server");

    if let Some(tx) = SHUTDOWN_TX.write().unwrap_or_else(|p| p.into_inner()).take() {
        let _ = tx.send(());
    }

    // Named pipes are kernel objects — no file cleanup needed
    BADGE_SERVER_RUNNING.store(false, Ordering::Release);
    info!("Named Pipe badge server stopped");
}

#[cfg(not(any(unix, windows)))]
pub async fn stop_badge_server() {
    // No-op on unsupported platforms
}

/// Update state for a single file
pub async fn update_file_state(path: &Path, state: SyncBadgeState) {
    let path_buf = path.to_path_buf();

    {
        let mut tracker = BADGE_TRACKER.write().unwrap_or_else(|p| p.into_inner());
        tracker.set_state(path_buf.clone(), state);
    }

    notify_update(path).await;
}

/// Update state for all files in a directory (recursive)
pub async fn update_directory_state(dir: &Path, state: SyncBadgeState) {
    let dir_buf = dir.to_path_buf();

    {
        let mut tracker = BADGE_TRACKER.write().unwrap_or_else(|p| p.into_inner());

        let paths_to_update: Vec<PathBuf> = tracker
            .states
            .keys()
            .filter(|p| p.starts_with(&dir_buf))
            .cloned()
            .collect();

        for path in paths_to_update {
            tracker.set_state(path, state);
        }
    }

    notify_update(dir).await;
}

/// Get state for a specific file
pub async fn get_file_state(path: &Path) -> Option<SyncBadgeState> {
    let mut tracker = BADGE_TRACKER.write().unwrap_or_else(|p| p.into_inner());
    tracker.get_state(path)
}

/// Register a sync root directory
pub async fn register_sync_root(path: PathBuf) {
    {
        let mut tracker = BADGE_TRACKER.write().unwrap_or_else(|p| p.into_inner());
        tracker.add_sync_root(path.clone());
    }

    // On Windows, also register with Cloud Filter API for native Explorer badges
    #[cfg(windows)]
    {
        if let Err(e) = crate::cloud_filter_badge::register_cloud_sync_root(&path, "AeroCloud") {
            warn!("Cloud Filter registration failed for {:?}: {}", path, e);
        }
    }
}


/// Clear all tracked states
pub async fn clear_all_states() {
    let mut tracker = BADGE_TRACKER.write().unwrap_or_else(|p| p.into_inner());
    tracker.clear_all();
}

// ============================================================================
// Unix Socket Server Implementation
// ============================================================================

#[cfg(unix)]
fn get_socket_path() -> Result<PathBuf, String> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();

    if let Some(runtime_dir) = runtime_dir {
        let socket_dir = PathBuf::from(runtime_dir).join("aerocloud");
        Ok(socket_dir.join("socket"))
    } else {
        // Fallback: use real UID (audit fix GB-004: was using PID)
        let uid = unsafe { libc::getuid() };
        let socket_dir = PathBuf::from(format!("/tmp/aerocloud-{}", uid));
        Ok(socket_dir.join("socket"))
    }
}

/// Unix socket client handler — delegates to generic handler
#[cfg(unix)]
async fn handle_client(
    stream: tokio::net::UnixStream,
    conn_id: u32,
    shutdown_rx: broadcast::Receiver<()>,
) {
    let (reader, writer) = stream.into_split();
    handle_client_generic(reader, writer, conn_id, shutdown_rx).await;
}

#[cfg(unix)]
async fn notify_update(path: &Path) {
    // Placeholder for future push notification to connected clients.
    // Currently clients poll via RETRIEVE_FILE_STATUS.
    let _ = path;
}

#[cfg(windows)]
async fn notify_update(path: &Path) {
    // Update Windows Cloud Filter sync state for Explorer badges
    if let Some(state) = {
        let mut tracker = BADGE_TRACKER.write().unwrap_or_else(|p| p.into_inner());
        tracker.get_state(path)
    } {
        if let Err(e) = crate::cloud_filter_badge::set_cloud_sync_state(path, state) {
            warn!("Cloud Filter badge update failed for {:?}: {}", path, e);
        }
    }
}

#[cfg(not(any(unix, windows)))]
async fn notify_update(_path: &Path) {
    // No-op on unsupported platforms
}

// ============================================================================
// GIO Emblem Support
// ============================================================================

/// Set GIO emblem for a file (pub(crate) — callers must pre-validate path)
#[cfg(target_os = "linux")]
pub(crate) fn set_gio_emblem(path: &Path, state: SyncBadgeState) -> Result<(), String> {
    let emblem_name = state.to_emblem_name();
    let path_str = path
        .to_str()
        .ok_or_else(|| "Invalid UTF-8 in path".to_string())?;

    let output = std::process::Command::new("gio")
        .args(["set", path_str, "-t", "stringv", "metadata::emblems", emblem_name])
        .output()
        .map_err(|e| format!("Failed to execute gio: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "gio set failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}

/// Clear GIO emblem for a file
#[cfg(target_os = "linux")]
pub(crate) fn clear_gio_emblem(path: &Path) -> Result<(), String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| "Invalid UTF-8 in path".to_string())?;

    let output = std::process::Command::new("gio")
        .args(["set", path_str, "-t", "unset", "metadata::emblems"])
        .output()
        .map_err(|e| format!("Failed to execute gio: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "gio set failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}

// ============================================================================
// Shell Extension Installation
// ============================================================================

/// Install Nautilus/Nemo Python extensions (Linux only)
#[cfg(target_os = "linux")]
pub fn install_shell_extension() -> Result<String, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let home_path = PathBuf::from(home);

    let nautilus_extension = include_str!("../resources/aerocloud_nautilus.py");
    let nemo_extension = include_str!("../resources/aerocloud_nemo.py");

    // Nautilus
    let nautilus_dir = home_path.join(".local/share/nautilus-python/extensions");
    std::fs::create_dir_all(&nautilus_dir)
        .map_err(|e| format!("Failed to create Nautilus dir: {}", e))?;

    let nautilus_file = nautilus_dir.join("aerocloud.py");
    std::fs::write(&nautilus_file, nautilus_extension)
        .map_err(|e| format!("Failed to write Nautilus extension: {}", e))?;

    // Set 0644 permissions (audit fix GB-011)
    let mut perms = std::fs::metadata(&nautilus_file)
        .map_err(|e| format!("metadata: {}", e))?
        .permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&nautilus_file, perms)
        .map_err(|e| format!("permissions: {}", e))?;

    // Nemo
    let nemo_dir = home_path.join(".local/share/nemo-python/extensions");
    std::fs::create_dir_all(&nemo_dir)
        .map_err(|e| format!("Failed to create Nemo dir: {}", e))?;

    let nemo_file = nemo_dir.join("aerocloud.py");
    std::fs::write(&nemo_file, nemo_extension)
        .map_err(|e| format!("Failed to write Nemo extension: {}", e))?;

    let mut perms = std::fs::metadata(&nemo_file)
        .map_err(|e| format!("metadata: {}", e))?
        .permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&nemo_file, perms)
        .map_err(|e| format!("permissions: {}", e))?;

    // Install emblems
    install_emblems(&home_path)?;

    Ok("Shell extensions installed successfully! Click \"Restart File Manager\" below to activate badges, or restart it manually later.".to_string())
}

/// Install FinderSync extension guide (macOS)
#[cfg(target_os = "macos")]
pub fn install_shell_extension() -> Result<String, String> {
    Ok("FinderSync extension is bundled with AeroFTP. Enable it in System Settings > Extensions > Finder Extensions > AeroFTP Finder Sync. The extension communicates with AeroFTP via Unix socket to display sync status badges in Finder.".to_string())
}

#[cfg(windows)]
pub fn install_shell_extension() -> Result<String, String> {
    Ok("Sync badges are managed automatically via Windows Cloud Filter API. No manual installation needed.".to_string())
}

#[cfg(not(any(unix, windows)))]
pub fn install_shell_extension() -> Result<String, String> {
    Err("Shell extensions not supported on this platform".to_string())
}

/// Uninstall Nautilus/Nemo shell extensions (Linux only)
#[cfg(target_os = "linux")]
pub fn uninstall_shell_extension() -> Result<String, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let home_path = PathBuf::from(home);

    let nautilus_file = home_path
        .join(".local/share/nautilus-python/extensions")
        .join("aerocloud.py");
    if nautilus_file.exists() {
        std::fs::remove_file(&nautilus_file)
            .map_err(|e| format!("Failed to remove Nautilus extension: {}", e))?;
    }

    let nemo_file = home_path
        .join(".local/share/nemo-python/extensions")
        .join("aerocloud.py");
    if nemo_file.exists() {
        std::fs::remove_file(&nemo_file)
            .map_err(|e| format!("Failed to remove Nemo extension: {}", e))?;
    }

    uninstall_emblems(&home_path)?;

    Ok("Shell extensions removed. Click \"Restart File Manager\" below or restart it manually.".to_string())
}

/// Disable FinderSync extension guide (macOS)
#[cfg(target_os = "macos")]
pub fn uninstall_shell_extension() -> Result<String, String> {
    Ok("Disable AeroFTP Finder Sync in System Settings > Extensions > Finder Extensions. The extension will stop showing sync badges in Finder.".to_string())
}

#[cfg(windows)]
pub fn uninstall_shell_extension() -> Result<String, String> {
    crate::cloud_filter_badge::cleanup_all_roots()?;
    Ok("Cloud Filter sync roots deregistered. Explorer badges removed.".to_string())
}

#[cfg(not(any(unix, windows)))]
pub fn uninstall_shell_extension() -> Result<String, String> {
    Err("Shell extensions not supported on this platform".to_string())
}

#[cfg(target_os = "linux")]
fn install_emblems(home_path: &Path) -> Result<(), String> {
    let emblem_dir = home_path.join(".local/share/icons/hicolor/scalable/emblems");
    std::fs::create_dir_all(&emblem_dir)
        .map_err(|e| format!("Failed to create emblem dir: {}", e))?;

    let emblems = [
        ("emblem-aerocloud-synced.svg", EMBLEM_SYNCED_SVG),
        ("emblem-aerocloud-syncing.svg", EMBLEM_SYNCING_SVG),
        ("emblem-aerocloud-error.svg", EMBLEM_ERROR_SVG),
        ("emblem-aerocloud-ignored.svg", EMBLEM_IGNORED_SVG),
        ("emblem-aerocloud-conflict.svg", EMBLEM_CONFLICT_SVG),
        ("emblem-aerocloud-new.svg", EMBLEM_NEW_SVG),
    ];

    for (filename, content) in &emblems {
        let emblem_file = emblem_dir.join(filename);
        std::fs::write(&emblem_file, content)
            .map_err(|e| format!("Failed to write emblem {}: {}", filename, e))?;
    }

    let _ = std::process::Command::new("gtk-update-icon-cache")
        .arg(home_path.join(".local/share/icons/hicolor"))
        .output();

    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_emblems(home_path: &Path) -> Result<(), String> {
    let emblem_dir = home_path.join(".local/share/icons/hicolor/scalable/emblems");

    let emblems = [
        "emblem-aerocloud-synced.svg",
        "emblem-aerocloud-syncing.svg",
        "emblem-aerocloud-error.svg",
        "emblem-aerocloud-ignored.svg",
        "emblem-aerocloud-conflict.svg",
        "emblem-aerocloud-new.svg",
    ];

    for filename in &emblems {
        let emblem_file = emblem_dir.join(filename);
        if emblem_file.exists() {
            std::fs::remove_file(&emblem_file)
                .map_err(|e| format!("Failed to remove emblem {}: {}", filename, e))?;
        }
    }

    let _ = std::process::Command::new("gtk-update-icon-cache")
        .arg(home_path.join(".local/share/icons/hicolor"))
        .output();

    Ok(())
}

// Emblem SVG content (minimal 16x16 icons)
const EMBLEM_SYNCED_SVG: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg width="16" height="16" version="1.1" xmlns="http://www.w3.org/2000/svg">
  <circle cx="8" cy="8" r="6" fill="#10b981"/>
  <path d="M5 8 L7 10 L11 6" stroke="white" stroke-width="2" fill="none"/>
</svg>"##;

const EMBLEM_SYNCING_SVG: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg width="16" height="16" version="1.1" xmlns="http://www.w3.org/2000/svg">
  <circle cx="8" cy="8" r="6" fill="#3b82f6"/>
  <path d="M8 4 L8 8 L11 8" stroke="white" stroke-width="2" fill="none"/>
</svg>"##;

const EMBLEM_ERROR_SVG: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg width="16" height="16" version="1.1" xmlns="http://www.w3.org/2000/svg">
  <circle cx="8" cy="8" r="6" fill="#ef4444"/>
  <path d="M6 6 L10 10 M10 6 L6 10" stroke="white" stroke-width="2"/>
</svg>"##;

const EMBLEM_IGNORED_SVG: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg width="16" height="16" version="1.1" xmlns="http://www.w3.org/2000/svg">
  <circle cx="8" cy="8" r="6" fill="#6b7280"/>
  <path d="M6 8 L10 8" stroke="white" stroke-width="2"/>
</svg>"##;

const EMBLEM_CONFLICT_SVG: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg width="16" height="16" version="1.1" xmlns="http://www.w3.org/2000/svg">
  <circle cx="8" cy="8" r="6" fill="#f59e0b"/>
  <path d="M8 5 L8 9 M8 11 L8 12" stroke="white" stroke-width="2"/>
</svg>"##;

const EMBLEM_NEW_SVG: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg width="16" height="16" version="1.1" xmlns="http://www.w3.org/2000/svg">
  <circle cx="8" cy="8" r="6" fill="#8b5cf6"/>
  <path d="M8 5 L8 11 M5 8 L11 8" stroke="white" stroke-width="2"/>
</svg>"##;

// ============================================================================
// Tauri Commands
// ============================================================================

#[tauri::command]
pub async fn start_badge_server_cmd(app: tauri::AppHandle) -> Result<String, String> {
    start_badge_server(app).await?;
    Ok("Badge server started".to_string())
}

#[tauri::command]
pub async fn stop_badge_server_cmd() -> Result<String, String> {
    stop_badge_server().await;
    Ok("Badge server stopped".to_string())
}

#[tauri::command]
pub async fn set_file_badge(path: String, state: String) -> Result<(), String> {
    let path_buf = validate_path(&path)?;

    let badge_state = match state.as_str() {
        "synced" => SyncBadgeState::Synced,
        "syncing" => SyncBadgeState::Syncing,
        "error" => SyncBadgeState::Error,
        "ignored" => SyncBadgeState::Ignored,
        "conflict" => SyncBadgeState::Conflict,
        "new" => SyncBadgeState::New,
        _ => return Err(format!("Invalid state: {}", state)),
    };

    update_file_state(&path_buf, badge_state).await;

    // Also set GIO emblem as fallback (Linux only — gio command doesn't exist on macOS/Windows)
    #[cfg(target_os = "linux")]
    { let _ = set_gio_emblem(&path_buf, badge_state); }

    Ok(())
}

#[tauri::command]
pub async fn clear_file_badge(path: String) -> Result<(), String> {
    let path_buf = validate_path(&path)?;

    {
        let mut tracker = BADGE_TRACKER.write().unwrap_or_else(|p| p.into_inner());
        tracker.remove_state(&path_buf);
    }

    #[cfg(target_os = "linux")]
    { let _ = clear_gio_emblem(&path_buf); }

    notify_update(&path_buf).await;
    Ok(())
}

#[tauri::command]
pub async fn get_badge_status(path: String) -> Result<String, String> {
    let path_buf = validate_path(&path)?;

    if let Some(state) = get_file_state(&path_buf).await {
        Ok(state.to_status_str().to_string())
    } else {
        Ok("NOP".to_string())
    }
}

#[tauri::command]
pub async fn install_shell_extension_cmd() -> Result<String, String> {
    install_shell_extension()
}

#[tauri::command]
pub async fn uninstall_shell_extension_cmd() -> Result<String, String> {
    uninstall_shell_extension()
}

/// Gracefully restart the user's file manager (Nautilus/Nemo) to pick up extension changes.
/// Uses `nautilus -q` / `nemo -q` which ask Nautilus/Nemo to quit cleanly
/// (the desktop environment auto-restarts them when a folder is opened).
#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn restart_file_manager_cmd() -> Result<String, String> {
    let mut restarted = Vec::new();

    // Try Nautilus (GNOME)
    if std::process::Command::new("nautilus")
        .arg("-q")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        restarted.push("Nautilus");
    }

    // Try Nemo (Cinnamon)
    if std::process::Command::new("nemo")
        .arg("-q")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        restarted.push("Nemo");
    }

    if restarted.is_empty() {
        Ok("No file manager found to restart. Open a folder to load the extensions.".to_string())
    } else {
        Ok(format!("{} restarted. Open a folder to see the badges!", restarted.join(" & ")))
    }
}

/// Restart Finder to reload FinderSync extension.
/// `killall Finder` is the standard macOS way to restart Finder (it auto-relaunches).
#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn restart_file_manager_cmd() -> Result<String, String> {
    let status = std::process::Command::new("killall")
        .arg("Finder")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => {
            Ok("Finder restarted. FinderSync extension will reload automatically.".to_string())
        }
        _ => {
            Ok("Could not restart Finder. Open a Finder window to reload the extension.".to_string())
        }
    }
}

/// Refresh Windows Explorer shell to pick up badge changes via SHChangeNotify.
#[cfg(windows)]
#[tauri::command]
pub async fn restart_file_manager_cmd() -> Result<String, String> {
    use windows::Win32::UI::Shell::SHChangeNotify;
    use windows::Win32::UI::Shell::SHCNE_ASSOCCHANGED;
    use windows::Win32::UI::Shell::SHCNF_IDLIST;

    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }

    Ok("Explorer refreshed. Sync badges will update automatically.".to_string())
}

#[cfg(not(any(unix, windows)))]
#[tauri::command]
pub async fn restart_file_manager_cmd() -> Result<String, String> {
    Err("File manager restart not supported on this platform".to_string())
}
