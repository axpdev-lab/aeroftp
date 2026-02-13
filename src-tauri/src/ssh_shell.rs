//! SSH Remote Shell module
//!
//! Opens interactive shell sessions over SSH to remote servers.
//! Reuses the same russh library and authentication flow as the SFTP provider.
//! Each shell session has its own SSH connection.

use russh::client::{self, Config, Handle, Handler, Msg};
use russh::keys::{self, known_hosts, PrivateKeyWithHashAlg, PublicKey};
use russh::client::AuthResult;
use russh::{Channel, ChannelId, ChannelMsg, CryptoVec};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tauri::{AppHandle, Emitter, State};

/// SSH handler for shell connections (key verification reuses known_hosts)
struct ShellSshHandler {
    host: String,
    port: u16,
}

impl ShellSshHandler {
    fn new(host: &str, port: u16) -> Self {
        Self { host: host.to_string(), port }
    }
}

impl Handler for ShellSshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        match known_hosts::check_known_hosts(&self.host, self.port, server_public_key) {
            Ok(true) => Ok(true),
            Ok(false) => {
                if let Err(e) = known_hosts::learn_known_hosts(&self.host, self.port, server_public_key) {
                    tracing::warn!("SSH Shell: Failed to save host key: {}", e);
                }
                Ok(true)
            }
            Err(keys::Error::KeyChanged { line }) => {
                tracing::error!(
                    "SSH Shell: REJECTING {} - host key changed at line {} (possible MITM)",
                    self.host, line
                );
                Ok(false)
            }
            Err(e) => {
                // SEC: Reject on unknown errors — do not silently accept.
                tracing::error!(
                    "SSH Shell: REJECTING {} - known_hosts verification error: {}",
                    self.host, e
                );
                Ok(false)
            }
        }
    }
}

/// An SSH shell session with handle for writing and channel ID
struct SshShellSession {
    handle: Handle<ShellSshHandler>,
    channel_id: ChannelId,
}

/// Global state for SSH shell sessions
pub struct SshShellState {
    sessions: HashMap<String, SshShellSession>,
    next_id: u64,
}

impl Default for SshShellState {
    fn default() -> Self {
        Self { sessions: HashMap::new(), next_id: 1 }
    }
}

pub type SshShellManager = Arc<Mutex<SshShellState>>;

pub fn create_ssh_shell_state() -> SshShellManager {
    Arc::new(Mutex::new(SshShellState::default()))
}

/// Open an SSH shell session to a remote server
#[tauri::command]
pub async fn ssh_shell_open(
    app: AppHandle,
    state: State<'_, SshShellManager>,
    host: String,
    port: u16,
    username: String,
    password: Option<String>,
    private_key_path: Option<String>,
    key_passphrase: Option<String>,
) -> Result<String, String> {
    let config = Config {
        inactivity_timeout: Some(std::time::Duration::from_secs(120)),
        keepalive_interval: Some(std::time::Duration::from_secs(15)),
        keepalive_max: 3,
        ..Default::default()
    };

    let addr = format!("{}:{}", host, port);
    let mut handle = client::connect(
        Arc::new(config),
        &addr,
        ShellSshHandler::new(&host, port),
    )
    .await
    .map_err(|e| format!("SSH connect failed: {}", e))?;

    // Authenticate
    let mut authenticated = false;

    // Try key-based auth first
    if let Some(ref key_path) = private_key_path {
        let path = std::path::Path::new(key_path);
        if path.exists() {
            let passphrase = key_passphrase.as_deref();
            if let Ok(key_data) = std::fs::read_to_string(path) {
                if let Ok(key) = keys::decode_secret_key(&key_data, passphrase) {
                    let key_pair = PrivateKeyWithHashAlg::new(Arc::new(key), None);
                    if let Ok(AuthResult::Success) = handle.authenticate_publickey(&username, key_pair).await {
                        authenticated = true;
                    }
                }
            }
        }
    }

    // Try password auth
    if !authenticated {
        if let Some(ref pw) = password {
            if let Ok(AuthResult::Success) = handle.authenticate_password(&username, pw).await {
                authenticated = true;
            }
        }
    }

    if !authenticated {
        return Err("SSH authentication failed".to_string());
    }

    // Open shell channel with PTY
    let channel = handle.channel_open_session().await
        .map_err(|e| format!("Open session: {}", e))?;

    channel.request_pty(true, "xterm-256color", 80, 24, 0, 0, &[])
        .await
        .map_err(|e| format!("Request PTY: {}", e))?;

    channel.request_shell(true).await
        .map_err(|e| format!("Request shell: {}", e))?;

    let channel_id = channel.id();

    // Generate session ID
    let session_id = {
        let mut mgr = state.lock().await;
        let id = format!("ssh-shell-{}", mgr.next_id);
        mgr.next_id += 1;
        id
    };

    let event_name = format!("pty-output-{}", session_id);
    let close_event = format!("ssh-shell-closed-{}", session_id);

    // Store handle + channel_id for writing
    {
        let mut mgr = state.lock().await;
        mgr.sessions.insert(session_id.clone(), SshShellSession {
            handle,
            channel_id,
        });
    }

    // Spawn read task — channel is consumed here, writing goes through Handle.data()
    let app_clone = app.clone();
    let session_id_clone = session_id.clone();
    let state_clone = state.inner().clone();
    tokio::spawn(async move {
        read_channel(channel, &app_clone, &event_name).await;
        let _ = app_clone.emit(&close_event, "closed");
        let mut mgr = state_clone.lock().await;
        mgr.sessions.remove(&session_id_clone);
    });

    Ok(format!("SSH shell opened [session:{}]", session_id))
}

/// Read loop for an SSH shell channel
async fn read_channel(mut channel: Channel<Msg>, app: &AppHandle, event_name: &str) {
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => {
                let text = String::from_utf8_lossy(&data).to_string();
                let _ = app.emit(event_name, text);
            }
            Some(ChannelMsg::ExtendedData { data, .. }) => {
                let text = String::from_utf8_lossy(&data).to_string();
                let _ = app.emit(event_name, text);
            }
            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                break;
            }
            _ => {}
        }
    }
}

/// Write data to an SSH shell session
#[tauri::command]
pub async fn ssh_shell_write(
    state: State<'_, SshShellManager>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    let mgr = state.lock().await;
    let session = mgr.sessions.get(&session_id)
        .ok_or("SSH shell session not found")?;

    let cv = CryptoVec::from(data.as_bytes());
    session.handle.data(session.channel_id, cv)
        .await
        .map_err(|_| "SSH write error".to_string())?;

    Ok(())
}

/// Resize an SSH shell session PTY
///
/// Note: russh Handle doesn't expose window_change directly.
/// Resize is handled by opening a new PTY request or is a no-op
/// if the server doesn't support it through the current API.
#[tauri::command]
pub async fn ssh_shell_resize(
    _state: State<'_, SshShellManager>,
    _session_id: String,
    _cols: u32,
    _rows: u32,
) -> Result<(), String> {
    // window_change is only available on Channel/ChannelWriteHalf which is consumed by the read task.
    // This is a known limitation — the terminal will use the initial 80x24 size.
    // A future refactor could use Channel::split() if russh exposes ChannelWriteHalf publicly.
    Ok(())
}

/// Close an SSH shell session
#[tauri::command]
pub async fn ssh_shell_close(
    state: State<'_, SshShellManager>,
    session_id: String,
) -> Result<(), String> {
    let mut mgr = state.lock().await;
    if let Some(session) = mgr.sessions.remove(&session_id) {
        let _ = session.handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    }
    Ok(())
}
