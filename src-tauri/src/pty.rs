// PTY (Pseudo-Terminal) module for real shell integration
// Uses portable-pty for cross-platform support (Linux/macOS/Windows)

use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::{AppHandle, Emitter, State};

/// Holds the PTY pair (master/slave) for the terminal session
pub struct PtySession {
    pub pair: Option<PtyPair>,
    pub writer: Option<Box<dyn Write + Send>>,
}

impl Default for PtySession {
    fn default() -> Self {
        Self {
            pair: None, 
            writer: None,
        }
    }
}

/// Global PTY state wrapped in Arc<Mutex>
pub type PtyState = Arc<Mutex<PtySession>>;

/// Create a new PTY state
pub fn create_pty_state() -> PtyState {
    Arc::new(Mutex::new(PtySession::default()))
}

/// Spawn a new shell in the PTY
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
    let shell = "powershell.exe".to_string();

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
    // We clone the reader to move it into a separate thread
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("Failed to clone reader: {}", e))?;

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("Failed to take writer: {}", e))?;

    // Store in state
    let mut session = pty_state.lock().map_err(|_| "Lock error")?;
    session.pair = Some(pair);
    session.writer = Some(writer);

    // Spawn a thread to read valid output from the PTY and emit it to the frontend
    // This avoids blocking the main thread or locking the state during read
    let app_clone = app.clone(); // Clone app handle for thread
    thread::spawn(move || {
        let mut buffer = [0u8; 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    let output = String::from_utf8_lossy(&buffer[..n]).to_string();
                    let _ = app_clone.emit("pty-output", output);
                }
                Err(_) => break, // Error or closed
            }
        }
    });

    Ok(format!("Shell started: {}", shell))
}

/// Write data to the PTY (send keystrokes to shell)
#[tauri::command]
pub fn pty_write(pty_state: State<'_, PtyState>, data: String) -> Result<(), String> {
    let mut session = pty_state.lock().map_err(|_| "Lock error")?;
    
    if let Some(ref mut writer) = session.writer {
        writer
            .write_all(data.as_bytes())
            .map_err(|e| format!("Write error: {}", e))?;
        writer.flush().map_err(|e| format!("Flush error: {}", e))?;
        Ok(())
    } else {
        Err("No active PTY session".to_string())
    }
}

// NOTE: pty_read is no longer needed as we use event emission

/// Resize the PTY
#[tauri::command]
pub fn pty_resize(pty_state: State<'_, PtyState>, rows: u16, cols: u16) -> Result<(), String> {
    let session = pty_state.lock().map_err(|_| "Lock error")?;
    
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
        Err("No active PTY session".to_string())
    }
}

/// Close the PTY session
#[tauri::command]
pub fn pty_close(pty_state: State<'_, PtyState>) -> Result<(), String> {
    let mut session = pty_state.lock().map_err(|_| "Lock error")?;
    
    // Dropping the pair should close the master/slave and terminate the shell
    // This will also cause the read thread to hit EOF or error and exit
    session.pair = None;
    session.writer = None;
    
    Ok(())
}
