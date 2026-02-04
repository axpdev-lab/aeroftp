// PTY (Pseudo-Terminal) module for real shell integration
// Uses portable-pty for cross-platform support (Linux/macOS/Windows)
// Supports multiple concurrent sessions (one per terminal tab)

use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::{AppHandle, Emitter, State};

/// Holds the PTY pair (master/slave) for a single terminal session
pub struct PtySession {
    pub pair: Option<PtyPair>,
    pub writer: Option<Box<dyn Write + Send>>,
}

/// Manager holding multiple PTY sessions keyed by session ID
pub struct PtyManager {
    pub sessions: HashMap<String, PtySession>,
    next_id: u64,
}

impl Default for PtyManager {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            next_id: 1,
        }
    }
}

impl PtyManager {
    fn next_session_id(&mut self) -> String {
        let id = format!("pty-{}", self.next_id);
        self.next_id += 1;
        id
    }
}

/// Global PTY state wrapped in Arc<Mutex>
pub type PtyState = Arc<Mutex<PtyManager>>;

/// Create a new PTY state
pub fn create_pty_state() -> PtyState {
    Arc::new(Mutex::new(PtyManager::default()))
}

/// Spawn a new shell in the PTY. Returns session info including session ID.
#[tauri::command]
pub fn spawn_shell(app: AppHandle, pty_state: State<'_, PtyState>, cwd: Option<String>) -> Result<String, String> {
    let pty_system = native_pty_system();

    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("Failed to open PTY: {}", e))?;

    // Determine the shell to use
    #[cfg(unix)]
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());

    #[cfg(windows)]
    let shell = {
        // Prefer PowerShell, fall back to cmd.exe
        let ps = std::env::var("SystemRoot")
            .map(|sr| format!("{}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", sr))
            .unwrap_or_else(|_| "powershell.exe".to_string());
        if std::path::Path::new(&ps).exists() {
            ps
        } else {
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
        }
    };

    let mut cmd = CommandBuilder::new(&shell);

    // Set environment variables for better terminal experience
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("FORCE_COLOR", "1");
    cmd.env("CLICOLOR", "1");
    cmd.env("CLICOLOR_FORCE", "1");

    // Unix: set a colorful PS1 prompt (bash/zsh)
    #[cfg(unix)]
    cmd.env("PS1", r"\[\e[1;36m\]\u@\h\[\e[0m\]:\[\e[1;34m\]\w\[\e[0m\]\$ ");

    // Set working directory
    if let Some(path) = cwd {
        cmd.cwd(path);
    } else if let Ok(current) = std::env::current_dir() {
        cmd.cwd(current);
    }

    // Spawn the shell
    let _child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("Failed to spawn shell: {}", e))?;

    // Get reader and writer from master
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("Failed to clone reader: {}", e))?;

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("Failed to take writer: {}", e))?;

    // Generate session ID
    let mut manager = pty_state.lock().map_err(|_| "Lock error")?;
    let session_id = manager.next_session_id();

    // Store in state
    manager.sessions.insert(session_id.clone(), PtySession {
        pair: Some(pair),
        writer: Some(writer),
    });

    // Spawn a thread to read output from the PTY and emit it to the frontend
    // Each session emits to its own event channel: pty-output-{session_id}
    let app_clone = app.clone();
    let event_name = format!("pty-output-{}", session_id);
    thread::spawn(move || {
        let mut buffer = [0u8; 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    let output = String::from_utf8_lossy(&buffer[..n]).to_string();
                    let _ = app_clone.emit(&event_name, output);
                }
                Err(_) => break, // Error or closed
            }
        }
    });

    Ok(format!("Shell started: {} [session:{}]", shell, session_id))
}

/// Write data to a PTY session (send keystrokes to shell)
#[tauri::command]
pub fn pty_write(pty_state: State<'_, PtyState>, data: String, session_id: Option<String>) -> Result<(), String> {
    let mut manager = pty_state.lock().map_err(|_| "Lock error")?;

    // Find the session — use provided ID or fall back to the most recent
    let session = if let Some(ref id) = session_id {
        manager.sessions.get_mut(id)
    } else {
        // Legacy fallback: use last session
        manager.sessions.values_mut().last()
    };

    if let Some(session) = session {
        if let Some(ref mut writer) = session.writer {
            writer
                .write_all(data.as_bytes())
                .map_err(|e| format!("Write error: {}", e))?;
            writer.flush().map_err(|e| format!("Flush error: {}", e))?;
            Ok(())
        } else {
            Err("No writer for PTY session".to_string())
        }
    } else {
        Err("No active PTY session".to_string())
    }
}

/// Resize a PTY session
#[tauri::command]
pub fn pty_resize(pty_state: State<'_, PtyState>, rows: u16, cols: u16, session_id: Option<String>) -> Result<(), String> {
    let manager = pty_state.lock().map_err(|_| "Lock error")?;

    let session = if let Some(ref id) = session_id {
        manager.sessions.get(id)
    } else {
        manager.sessions.values().last()
    };

    if let Some(session) = session {
        if let Some(ref pair) = session.pair {
            pair.master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| format!("Resize error: {}", e))?;
            Ok(())
        } else {
            Err("No active PTY pair".to_string())
        }
    } else {
        Err("No active PTY session".to_string())
    }
}

/// Close a PTY session
#[tauri::command]
pub fn pty_close(pty_state: State<'_, PtyState>, session_id: Option<String>) -> Result<(), String> {
    let mut manager = pty_state.lock().map_err(|_| "Lock error")?;

    if let Some(id) = session_id {
        manager.sessions.remove(&id);
    } else {
        // Legacy: close the last session
        if let Some(last_key) = manager.sessions.keys().last().cloned() {
            manager.sessions.remove(&last_key);
        }
    }

    Ok(())
}
