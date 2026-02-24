//! AI Tool Execution via StorageProvider trait + FTP fallback
//!
//! Provides a unified `execute_ai_tool` command that routes AI tool calls
//! through the active StorageProvider (14 protocols). When no provider is
//! connected, falls back to `AppState.ftp_manager` for FTP/FTPS sessions.

use serde_json::{json, Value};
use tauri::{Emitter, State};
use tokio::process::Command as TokioCommand;
use std::process::Stdio;
use crate::provider_commands::ProviderState;
use crate::AppState;

/// Allowed tool names (whitelist)
const ALLOWED_TOOLS: &[&str] = &[
    "remote_list", "remote_read", "remote_upload", "remote_download",
    "remote_delete", "remote_rename", "remote_mkdir", "remote_search",
    "remote_info", "local_list", "local_read", "local_write",
    "local_mkdir", "local_delete", "local_rename", "local_search", "local_edit",
    "local_move_files", "local_batch_rename", "local_copy_files", "local_trash",
    "local_file_info", "local_disk_usage", "local_find_duplicates",
    "remote_edit",
    // Batch transfer tools
    "upload_files", "download_files",
    // Advanced tools
    "sync_preview", "archive_compress", "archive_decompress",
    // RAG tools
    "rag_index", "rag_search",
    // Preview tools
    "preview_edit",
    // Agent memory
    "agent_memory_write",
    // Cyber tools
    "hash_file",
    // Content inspection tools
    "local_grep", "local_head", "local_tail", "local_stat_batch",
    "local_diff", "local_tree",
    // Clipboard tools
    "clipboard_read", "clipboard_write",
    // App control tools
    "set_theme", "app_info", "sync_control", "vault_peek",
    // Shell execution
    "shell_execute",
];

/// Validate a path argument — reject null bytes, traversal, excessive length
fn validate_path(path: &str, param: &str) -> Result<(), String> {
    if path.len() > 4096 {
        return Err(format!("{}: path exceeds 4096 characters", param));
    }
    if path.contains('\0') {
        return Err(format!("{}: path contains null bytes", param));
    }
    let normalized = path.replace('\\', "/");
    for component in normalized.split('/') {
        if component == ".." {
            return Err(format!("{}: path traversal ('..') not allowed", param));
        }
    }
    // Resolve symlinks and verify canonical path is not a sensitive system path.
    // For non-existent files, check the parent directory to avoid write/read inconsistency.
    let resolved = std::fs::canonicalize(path).or_else(|_| {
        std::path::Path::new(path)
            .parent()
            .map(std::fs::canonicalize)
            .unwrap_or(Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no parent")))
    });
    if let Ok(canonical) = resolved {
        let s = canonical.to_string_lossy();
        // Block sensitive system paths (deny-list)
        let denied = [
            "/proc", "/sys", "/dev", "/boot", "/root",
            "/etc/shadow", "/etc/passwd", "/etc/ssh", "/etc/sudoers",
        ];
        if denied.iter().any(|d| s.starts_with(d)) {
            return Err(format!("{}: access to system path denied: {}", param, s));
        }
        // Block sensitive home-relative paths
        if let Ok(home) = std::env::var("HOME") {
            let home_denied = [".ssh", ".gnupg", ".aws", ".kube", ".config/gcloud"];
            for sensitive in &home_denied {
                if s.starts_with(&format!("{}/{}", home, sensitive)) {
                    return Err(format!("{}: access to sensitive path denied: {}", param, s));
                }
            }
        }
    }
    Ok(())
}

fn get_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("Missing required argument: {}", key))
}

fn get_str_opt(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Check if the StorageProvider is connected
async fn has_provider(state: &ProviderState) -> bool {
    state.provider.lock().await.is_some()
}

/// Check if FTP manager has an active connection
async fn has_ftp(app_state: &AppState) -> bool {
    app_state.ftp_manager.lock().await.is_connected()
}

/// Emit tool progress event for iterative operations
fn emit_tool_progress(app: &tauri::AppHandle, tool: &str, current: u32, total: u32, item: &str) {
    let _ = app.emit("ai-tool-progress", json!({
        "tool": tool,
        "current": current,
        "total": total,
        "item": item,
    }));
}

const MAX_AI_DOWNLOAD_SIZE: u64 = 50 * 1024 * 1024; // 50MB

/// Download a remote file to bytes via StorageProvider or FTP fallback
async fn download_from_provider(
    state: &State<'_, ProviderState>,
    app_state: &State<'_, AppState>,
    path: &str,
) -> Result<Vec<u8>, String> {
    if has_provider(state).await {
        let mut provider = state.provider.lock().await;
        let provider = match provider.as_mut() {
            Some(p) => p,
            None => return Err("No active provider connection".into()),
        };
        // Check file size before downloading
        if let Ok(entry) = provider.stat(path).await {
            if entry.size > MAX_AI_DOWNLOAD_SIZE {
                return Err(format!(
                    "File too large to download ({:.1} MB). Limit is {} MB.",
                    entry.size as f64 / 1_048_576.0,
                    MAX_AI_DOWNLOAD_SIZE / 1_048_576
                ));
            }
        }
        provider.download_to_bytes(path).await.map_err(|e| e.to_string())
    } else if has_ftp(app_state).await {
        let mut manager = app_state.ftp_manager.lock().await;
        manager.download_to_bytes(path).await.map_err(|e| e.to_string())
    } else {
        Err("Not connected to any server".to_string())
    }
}

#[tauri::command]
pub async fn validate_tool_args(
    tool_name: String,
    args: Value,
) -> Result<Value, String> {
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Path validation for all tools with path args
    for key in &["path", "local_path", "remote_path", "from", "to"] {
        if let Some(path) = args.get(key).and_then(|v| v.as_str()) {
            if let Err(e) = validate_path(path, key) {
                errors.push(e);
            }
        }
    }

    // Tool-specific validation
    match tool_name.as_str() {
        "local_read" | "local_edit" | "local_search" | "local_list" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if tool_name == "local_list" || tool_name == "local_search" {
                    if !p.is_dir() {
                        errors.push(format!("Directory not found: {}", path));
                    }
                } else if !p.exists() {
                    errors.push(format!("File not found: {}", path));
                } else if p.is_dir() {
                    errors.push(format!("Path is a directory, not a file: {}", path));
                } else if let Ok(meta) = p.metadata() {
                    let size = meta.len();
                    if size > 5_242_880 {
                        warnings.push(format!(
                            "File is large ({:.1} MB). Edit operations may be slow.",
                            size as f64 / 1_048_576.0
                        ));
                    }
                    // Check read-only for edit tools
                    if tool_name == "local_edit" && meta.permissions().readonly() {
                        errors.push(format!("File is read-only: {}", path));
                    }
                }
            }
            // Check find string for local_edit
            if tool_name == "local_edit" {
                if let Some(find) = args.get("find").and_then(|v| v.as_str()) {
                    if find.is_empty() {
                        errors.push("'find' parameter cannot be empty".to_string());
                    }
                }
            }
        }
        "local_write" | "local_mkdir" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                // Check parent exists
                if let Some(parent) = p.parent() {
                    if !parent.exists() {
                        warnings.push(format!(
                            "Parent directory does not exist: {}",
                            parent.display()
                        ));
                    }
                }
                // Check if path is read-only
                if p.exists() {
                    if let Ok(meta) = p.metadata() {
                        if meta.permissions().readonly() {
                            errors.push(format!("File is read-only: {}", path));
                        }
                    }
                }
            }
        }
        "local_delete" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let home_dir = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_default();
                let dangerous = ["/", "~", ".", "..", home_dir.as_str()];
                let normalized = path.trim_end_matches('/');
                if dangerous
                    .iter()
                    .any(|d| normalized == *d || normalized.is_empty())
                {
                    errors.push(format!("Refusing to delete dangerous path: {}", path));
                }
                let p = std::path::Path::new(path);
                if !p.exists() {
                    warnings.push(format!(
                        "Path does not exist (nothing to delete): {}",
                        path
                    ));
                }
            }
        }
        "local_move_files" => {
            let paths = args.get("paths").and_then(|v| v.as_array());
            if paths.is_none() || paths.is_some_and(|a| a.is_empty()) {
                errors.push("'paths' array is missing or empty".to_string());
            } else if let Some(arr) = paths {
                for p in arr.iter().filter_map(|v| v.as_str()) {
                    let path = std::path::Path::new(p);
                    if !path.exists() {
                        warnings.push(format!("Source file not found: {}", p));
                    }
                }
            }
            if let Some(dest) = args.get("destination").and_then(|v| v.as_str()) {
                if let Err(e) = validate_path(dest, "destination") {
                    errors.push(e);
                }
            }
        }
        "local_batch_rename" | "local_copy_files" | "local_trash" => {
            let paths = args.get("paths").and_then(|v| v.as_array());
            if paths.is_none() || paths.is_some_and(|a| a.is_empty()) {
                errors.push("'paths' array is missing or empty".to_string());
            } else if let Some(arr) = paths {
                for p in arr.iter().filter_map(|v| v.as_str()) {
                    let path = std::path::Path::new(p);
                    if !path.exists() {
                        warnings.push(format!("Source not found: {}", p));
                    }
                }
            }
            if tool_name == "local_batch_rename" {
                if let Some(mode) = args.get("mode").and_then(|v| v.as_str()) {
                    if !["find_replace", "add_prefix", "add_suffix", "sequential"].contains(&mode) {
                        errors.push(format!("Invalid rename mode: {}. Use find_replace, add_prefix, add_suffix, or sequential", mode));
                    }
                } else {
                    errors.push("Missing 'mode' parameter".to_string());
                }
            }
            if tool_name == "local_copy_files" {
                if let Some(dest) = args.get("destination").and_then(|v| v.as_str()) {
                    if let Err(e) = validate_path(dest, "destination") {
                        errors.push(e);
                    }
                }
            }
        }
        "archive_compress" => {
            let paths = args.get("paths").and_then(|v| v.as_array());
            if paths.is_none() || paths.is_some_and(|a| a.is_empty()) {
                errors.push("'paths' array is missing or empty".to_string());
            }
            if let Some(output) = args.get("output_path").and_then(|v| v.as_str()) {
                if let Err(e) = validate_path(output, "output_path") {
                    errors.push(e);
                }
            } else {
                errors.push("Missing 'output_path' parameter".to_string());
            }
            if let Some(fmt) = args.get("format").and_then(|v| v.as_str()) {
                if !["zip", "7z", "tar", "tar.gz", "tar.bz2", "tar.xz"].contains(&fmt) {
                    errors.push(format!("Unsupported format: {}. Use zip, 7z, tar, tar.gz, tar.bz2, or tar.xz", fmt));
                }
            }
        }
        "archive_decompress" => {
            if let Some(path) = args.get("archive_path").and_then(|v| v.as_str()) {
                if let Err(e) = validate_path(path, "archive_path") {
                    errors.push(e);
                }
                let p = std::path::Path::new(path);
                if !p.exists() {
                    errors.push(format!("Archive not found: {}", path));
                }
            } else {
                errors.push("Missing 'archive_path' parameter".to_string());
            }
            if let Some(dir) = args.get("output_dir").and_then(|v| v.as_str()) {
                if let Err(e) = validate_path(dir, "output_dir") {
                    errors.push(e);
                }
            }
        }
        "hash_file" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if !p.exists() {
                    errors.push(format!("File not found: {}", path));
                } else if p.is_dir() {
                    errors.push(format!("Path is a directory, not a file: {}", path));
                }
            }
            if let Some(algo) = args.get("algorithm").and_then(|v| v.as_str()) {
                if !["md5", "sha1", "sha256", "sha512", "blake3"].contains(&algo) {
                    errors.push(format!("Unsupported algorithm: {}. Use md5, sha1, sha256, sha512, or blake3", algo));
                }
            }
        }
        "local_file_info" | "local_disk_usage" | "local_find_duplicates" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if !p.exists() {
                    errors.push(format!("Path not found: {}", path));
                }
                if (tool_name == "local_disk_usage" || tool_name == "local_find_duplicates") && !p.is_dir() {
                    errors.push(format!("Path is not a directory: {}", path));
                }
            }
        }
        "local_grep" | "local_tree" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if !p.is_dir() {
                    errors.push(format!("Directory not found: {}", path));
                }
            }
            if tool_name == "local_grep" {
                if let Some(pattern) = args.get("pattern").and_then(|v| v.as_str()) {
                    if regex::Regex::new(pattern).is_err() {
                        errors.push(format!("Invalid regex pattern: {}", pattern));
                    }
                } else {
                    errors.push("Missing 'pattern' parameter".to_string());
                }
            }
        }
        "local_head" | "local_tail" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if !p.exists() {
                    errors.push(format!("File not found: {}", path));
                } else if p.is_dir() {
                    errors.push(format!("Path is a directory, not a file: {}", path));
                }
            }
        }
        "local_stat_batch" => {
            let paths = args.get("paths").and_then(|v| v.as_array());
            if paths.is_none() || paths.is_some_and(|a| a.is_empty()) {
                errors.push("'paths' array is missing or empty".to_string());
            } else if let Some(arr) = paths {
                if arr.len() > 100 {
                    errors.push(format!("Too many paths: {} (max 100)", arr.len()));
                }
            }
        }
        "local_diff" => {
            for key in &["path_a", "path_b"] {
                if let Some(path) = args.get(key).and_then(|v| v.as_str()) {
                    if let Err(e) = validate_path(path, key) {
                        errors.push(e);
                    }
                    let p = std::path::Path::new(path);
                    if !p.exists() {
                        errors.push(format!("File not found: {}", path));
                    } else if p.is_dir() {
                        errors.push(format!("Path is a directory, not a file: {}", path));
                    }
                } else {
                    errors.push(format!("Missing '{}' parameter", key));
                }
            }
        }
        "clipboard_write" => {
            if args.get("content").and_then(|v| v.as_str()).is_none() {
                errors.push("Missing 'content' parameter".to_string());
            }
        }
        "set_theme" => {
            match args.get("theme").and_then(|v| v.as_str()) {
                Some(t) if ["light", "dark", "tokyo", "cyber"].contains(&t) => {},
                Some(t) => errors.push(format!("Invalid theme '{}'. Use: light, dark, tokyo, cyber", t)),
                None => errors.push("Missing required parameter 'theme'".to_string()),
            }
        }
        "sync_control" => {
            match args.get("action").and_then(|v| v.as_str()) {
                Some(a) if ["start", "stop", "status"].contains(&a) => {},
                Some(a) => errors.push(format!("Invalid action '{}'. Use: start, stop, status", a)),
                None => errors.push("Missing required parameter 'action'".to_string()),
            }
        }
        "vault_peek" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                if let Err(e) = validate_path(path, "path") {
                    errors.push(e);
                }
                if !path.ends_with(".aerovault") {
                    warnings.push("File does not have .aerovault extension".to_string());
                }
            } else {
                errors.push("Missing required parameter 'path'".to_string());
            }
        }
        // app_info needs no validation (no parameters)
        _ => {} // Remote tools: path format already validated above
    }

    Ok(json!({
        "valid": errors.is_empty(),
        "errors": errors,
        "warnings": warnings,
    }))
}

/// Resolve a path against an optional base directory.
/// If path is already absolute, returns it unchanged.
/// If path is relative (just a filename) and base is Some, joins them.
fn resolve_local_path(path: &str, base: Option<&str>) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    if let Some(base_dir) = base {
        if !base_dir.is_empty() {
            return format!("{}/{}", base_dir.trim_end_matches('/'), path);
        }
    }
    path.to_string()
}

/// Maximum output size from shell commands (512 KB)
const SHELL_MAX_OUTPUT_BYTES: usize = 512 * 1024;

/// Denied command patterns — defense-in-depth (also checked on frontend)
static DENIED_COMMAND_PATTERNS: std::sync::LazyLock<Vec<regex::Regex>> = std::sync::LazyLock::new(|| {
    [
        r"^\s*rm\s+(-[a-zA-Z]*)?.*\s+/\s*$",         // rm -rf /
        r"^\s*rm\s+(-[a-zA-Z]*f[a-zA-Z]*\s+)?-[a-zA-Z]*r.*\s+/\s*$",
        r"^\s*mkfs\b",                                  // mkfs (format disk)
        r"^\s*dd\s+.*of=/dev/",                          // dd to device
        r"^\s*shutdown\b",                               // shutdown
        r"^\s*reboot\b",                                 // reboot
        r"^\s*halt\b",                                   // halt
        r"^\s*init\s+[06]\b",                            // init 0/6
        r"^\s*:\(\)\s*\{\s*:\|:\s*&\s*\}\s*;\s*:",      // fork bomb
        r"^\s*>\s*/dev/sd[a-z]",                         // overwrite disk
        r"^\s*chmod\s+(-[a-zA-Z]*\s+)?777\s+/",         // chmod 777 /
        r"^\s*chown\s+.*\s+/\s*$",                       // chown /
        // L19 — belt+suspenders: also caught by meta-char filter, but explicit is better
        r"^\s*python3?\s+-c\b",                          // python -c (arbitrary code exec)
        r"\bcurl\b.*\|",                                 // curl piped to shell
        r"\bwget\b.*\|",                                 // wget piped to shell
        r"^\s*eval\s",                                   // eval (arbitrary execution)
        r"^\s*base64\s+(-d|--decode)\b",                 // base64 decode (obfuscation bypass)
    ]
    .iter()
    .filter_map(|p| regex::Regex::new(p).ok())
    .collect()
});

/// Read image from system clipboard via arboard (native, works on WebKitGTK).
/// Returns base64 PNG or null if no image in clipboard.
#[tauri::command]
pub fn clipboard_read_image() -> Result<Option<String>, String> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| format!("Clipboard init failed: {}", e))?;
    let img = match clipboard.get_image() {
        Ok(img) => img,
        Err(_) => return Ok(None), // No image in clipboard
    };

    // Encode RGBA as BMP-like format, then convert via canvas on frontend.
    // Simpler: encode as raw RGBA + dimensions as JSON, let frontend render via canvas.
    use base64::Engine;
    let rgba_base64 = base64::engine::general_purpose::STANDARD.encode(&img.bytes);
    Ok(Some(format!("{}:{}:{}", img.width, img.height, rgba_base64)))
}

/// Execute a shell command and capture output.
/// Used by AeroAgent's shell_execute tool.
#[tauri::command]
pub async fn shell_execute(
    command: String,
    working_dir: Option<String>,
    timeout_secs: Option<u64>,
) -> Result<Value, String> {
    if command.trim().is_empty() {
        return Err("No command specified".to_string());
    }

    // Defense-in-depth: reject shell meta-characters that enable denylist bypass
    // (pipes, subshells, backticks, semicolons, eval chains, base64 decode, etc.)
    let meta_chars = ['|', ';', '`', '$', '&', '(', ')', '{', '}'];
    if meta_chars.iter().any(|c| command.contains(*c)) {
        return Err(
            "Command contains shell meta-characters (|;&`$(){}). Use simple commands only."
                .to_string(),
        );
    }

    // Security: check denied commands
    if DENIED_COMMAND_PATTERNS.iter().any(|rx| rx.is_match(&command)) {
        return Err("Command blocked: potentially destructive system command".to_string());
    }

    // Determine working directory
    let cwd = working_dir
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "/tmp".to_string())
        });

    // Validate working directory exists
    let cwd_path = std::path::Path::new(&cwd);
    if !cwd_path.exists() || !cwd_path.is_dir() {
        return Err(format!("Working directory does not exist: {}", cwd));
    }

    let timeout = std::time::Duration::from_secs(timeout_secs.unwrap_or(30).min(120));

    // Build command with environment isolation
    let mut cmd = TokioCommand::new("sh");
    cmd.args(["-c", &command])
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();

    // Restore minimal safe environment
    for key in &["PATH", "HOME", "LANG", "LC_ALL", "TERM", "TMPDIR", "USER", "SHELL"] {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }

    let child = cmd.spawn().map_err(|e| format!("Failed to spawn shell: {}", e))?;

    // Wait with timeout
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("Shell process error: {}", e)),
        Err(_) => {
            return Ok(json!({
                "stdout": "",
                "stderr": format!("Command timed out after {}s", timeout.as_secs()),
                "exit_code": -1,
                "success": false,
                "timed_out": true,
                "command": command
            }));
        }
    };

    let exit_code = output.status.code().unwrap_or(-1);

    // Truncate output if too large
    let stdout_raw = &output.stdout[..output.stdout.len().min(SHELL_MAX_OUTPUT_BYTES)];
    let stderr_raw = &output.stderr[..output.stderr.len().min(SHELL_MAX_OUTPUT_BYTES)];

    let stdout = String::from_utf8_lossy(stdout_raw);
    let stderr = String::from_utf8_lossy(stderr_raw);

    let stdout_truncated = output.stdout.len() > SHELL_MAX_OUTPUT_BYTES;
    let stderr_truncated = output.stderr.len() > SHELL_MAX_OUTPUT_BYTES;

    let mut stdout_str = stdout.to_string();
    if stdout_truncated {
        stdout_str.push_str(&format!("\n[...truncated, {} bytes total]", output.stdout.len()));
    }
    let mut stderr_str = stderr.to_string();
    if stderr_truncated {
        stderr_str.push_str(&format!("\n[...truncated, {} bytes total]", output.stderr.len()));
    }

    Ok(json!({
        "stdout": stdout_str,
        "stderr": stderr_str,
        "exit_code": exit_code,
        "success": output.status.success(),
        "timed_out": false,
        "command": command
    }))
}

#[tauri::command]
pub async fn execute_ai_tool(
    app: tauri::AppHandle,
    state: State<'_, ProviderState>,
    app_state: State<'_, AppState>,
    tool_name: String,
    args: Value,
    context_local_path: Option<String>,
) -> Result<Value, String> {
    // Whitelist check
    if !ALLOWED_TOOLS.contains(&tool_name.as_str()) {
        return Err(format!("Unknown or disallowed tool: {}", tool_name));
    }

    match tool_name.as_str() {
        "remote_list" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            // Try provider first, fall back to FTP
            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                let entries = provider.list(&path).await.map_err(|e| e.to_string())?;

                let items: Vec<Value> = entries.iter().take(100).map(|e| json!({
                    "name": e.name,
                    "path": e.path,
                    "is_dir": e.is_dir,
                    "size": e.size,
                    "modified": e.modified,
                })).collect();

                Ok(json!({
                    "entries": items,
                    "total": entries.len(),
                    "truncated": entries.len() > 100,
                }))
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                // Navigate to path, list, then return
                manager.change_dir(&path).await.map_err(|e| e.to_string())?;
                let files = manager.list_files().await.map_err(|e| e.to_string())?;

                let items: Vec<Value> = files.iter().take(100).map(|f| json!({
                    "name": f.name,
                    "path": format!("{}/{}", path.trim_end_matches('/'), f.name),
                    "is_dir": f.is_dir,
                    "size": f.size,
                    "modified": f.modified,
                })).collect();

                Ok(json!({
                    "entries": items,
                    "total": files.len(),
                    "truncated": files.len() > 100,
                }))
            } else {
                Err("Not connected to any server".to_string())
            }
        }

        "remote_read" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            let bytes = download_from_provider(&state, &app_state, &path).await?;

            let max_bytes = 5120;
            let truncated = bytes.len() > max_bytes;
            let content = String::from_utf8_lossy(&bytes[..bytes.len().min(max_bytes)]).to_string();

            Ok(json!({ "content": content, "size": bytes.len(), "truncated": truncated }))
        }

        "remote_upload" => {
            let local_path = get_str(&args, "local_path")?;
            let remote_path = get_str(&args, "remote_path")?;
            validate_path(&local_path, "local_path")?;
            validate_path(&remote_path, "remote_path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                provider.upload(&local_path, &remote_path, None).await.map_err(|e| e.to_string())?;
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.upload_file(&local_path, &remote_path).await.map_err(|e| e.to_string())?;
            } else {
                return Err("Not connected to any server".to_string());
            }

            Ok(json!({ "success": true, "message": format!("Uploaded {} to {}", local_path, remote_path) }))
        }

        "remote_download" => {
            let remote_path = get_str(&args, "remote_path")?;
            let local_path = get_str(&args, "local_path")?;
            validate_path(&remote_path, "remote_path")?;
            validate_path(&local_path, "local_path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                provider.download(&remote_path, &local_path, None).await.map_err(|e| e.to_string())?;
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.download_file(&remote_path, &local_path).await.map_err(|e| e.to_string())?;
            } else {
                return Err("Not connected to any server".to_string());
            }

            Ok(json!({ "success": true, "message": format!("Downloaded {} to {}", remote_path, local_path) }))
        }

        "remote_delete" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                provider.delete(&path).await.map_err(|e| e.to_string())?;
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.remove(&path).await.map_err(|e| e.to_string())?;
            } else {
                return Err("Not connected to any server".to_string());
            }

            Ok(json!({ "success": true, "message": format!("Deleted {}", path) }))
        }

        "remote_rename" => {
            let from = get_str(&args, "from")?;
            let to = get_str(&args, "to")?;
            validate_path(&from, "from")?;
            validate_path(&to, "to")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                provider.rename(&from, &to).await.map_err(|e| e.to_string())?;
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.rename(&from, &to).await.map_err(|e| e.to_string())?;
            } else {
                return Err("Not connected to any server".to_string());
            }

            Ok(json!({ "success": true, "message": format!("Renamed {} to {}", from, to) }))
        }

        "remote_mkdir" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                provider.mkdir(&path).await.map_err(|e| e.to_string())?;
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.mkdir(&path).await.map_err(|e| e.to_string())?;
            } else {
                return Err("Not connected to any server".to_string());
            }

            Ok(json!({ "success": true, "message": format!("Created directory {}", path) }))
        }

        "remote_search" => {
            let path = get_str(&args, "path")?;
            let pattern = get_str(&args, "pattern")?;
            validate_path(&path, "path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                let results = provider.find(&path, &pattern).await.map_err(|e| e.to_string())?;

                let items: Vec<Value> = results.iter().take(100).map(|e| json!({
                    "name": e.name,
                    "path": e.path,
                    "is_dir": e.is_dir,
                    "size": e.size,
                })).collect();

                Ok(json!({
                    "results": items,
                    "total": results.len(),
                    "truncated": results.len() > 100,
                }))
            } else if has_ftp(&app_state).await {
                // FTP doesn't have native search — list directory and filter client-side
                let mut manager = app_state.ftp_manager.lock().await;
                manager.change_dir(&path).await.map_err(|e| e.to_string())?;
                let files = manager.list_files().await.map_err(|e| e.to_string())?;

                let pattern_lower = pattern.to_lowercase();
                let results: Vec<Value> = files.iter()
                    .filter(|f| f.name.to_lowercase().contains(&pattern_lower))
                    .take(100)
                    .map(|f| json!({
                        "name": f.name,
                        "path": format!("{}/{}", path.trim_end_matches('/'), f.name),
                        "is_dir": f.is_dir,
                        "size": f.size,
                    }))
                    .collect();

                let total = results.len();
                Ok(json!({
                    "results": results,
                    "total": total,
                    "truncated": false,
                    "note": "FTP search is limited to current directory listing with name filter",
                }))
            } else {
                Err("Not connected to any server".to_string())
            }
        }

        "remote_info" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                let entry = provider.stat(&path).await.map_err(|e| e.to_string())?;

                Ok(json!({
                    "name": entry.name,
                    "path": entry.path,
                    "is_dir": entry.is_dir,
                    "size": entry.size,
                    "modified": entry.modified,
                    "permissions": entry.permissions,
                    "owner": entry.owner,
                }))
            } else if has_ftp(&app_state).await {
                // FTP: list parent dir and find the entry
                let file_name = path.rsplit(['/', '\\']).next().unwrap_or(&path);
                let parent = if let Some(pos) = path.rfind(['/', '\\']) {
                    let p = &path[..pos];
                    if p.is_empty() { "/" } else { p }
                } else {
                    "/"
                };

                let mut manager = app_state.ftp_manager.lock().await;
                manager.change_dir(parent).await.map_err(|e| e.to_string())?;
                let files = manager.list_files().await.map_err(|e| e.to_string())?;

                let entry = files.iter().find(|f| f.name == file_name)
                    .ok_or_else(|| format!("File not found: {}", path))?;

                Ok(json!({
                    "name": entry.name,
                    "path": path,
                    "is_dir": entry.is_dir,
                    "size": entry.size,
                    "modified": entry.modified,
                }))
            } else {
                Err("Not connected to any server".to_string())
            }
        }

        "local_list" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;

            let entries: Vec<Value> = std::fs::read_dir(&path)
                .map_err(|e| format!("Failed to read directory: {}", e))?
                .filter_map(|e| e.ok())
                .take(100)
                .map(|e| {
                    let meta = e.metadata().ok();
                    json!({
                        "name": e.file_name().to_string_lossy(),
                        "is_dir": meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                        "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    })
                })
                .collect();

            Ok(json!({ "entries": entries }))
        }

        "local_search" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            let pattern = get_str(&args, "pattern")?;
            validate_path(&path, "path")?;

            let pattern_lower = pattern.to_lowercase();

            // Simple glob support: *.pdf → ends_with(".pdf"), test* → starts_with("test")
            let matcher: Box<dyn Fn(&str) -> bool> = if let Some(suffix) = pattern_lower.strip_prefix('*') {
                let suffix = suffix.to_string();
                Box::new(move |name: &str| name.ends_with(&suffix))
            } else if let Some(prefix) = pattern_lower.strip_suffix('*') {
                let prefix = prefix.to_string();
                Box::new(move |name: &str| name.starts_with(&prefix))
            } else if pattern_lower.contains('*') {
                let parts: Vec<String> = pattern_lower.split('*').map(String::from).collect();
                Box::new(move |name: &str| {
                    parts.iter().all(|part| name.contains(part.as_str()))
                })
            } else {
                let pat = pattern_lower.clone();
                Box::new(move |name: &str| name.contains(&pat))
            };

            let results: Vec<Value> = std::fs::read_dir(&path)
                .map_err(|e| format!("Failed to read directory: {}", e))?
                .filter_map(|e| e.ok())
                .filter(|e| matcher(&e.file_name().to_string_lossy().to_lowercase()))
                .take(100)
                .map(|e| {
                    let meta = e.metadata().ok();
                    json!({
                        "name": e.file_name().to_string_lossy(),
                        "is_dir": meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                        "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    })
                })
                .collect();

            Ok(json!({
                "results": results,
                "total": results.len(),
            }))
        }

        "local_read" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;

            let meta = std::fs::metadata(&path)
                .map_err(|e| format!("Failed to stat file: {}", e))?;
            if meta.len() > 10_485_760 {
                return Err(format!("File too large for local_read: {:.1} MB (max 10 MB)", meta.len() as f64 / 1_048_576.0));
            }

            // Only read the first 5KB instead of the entire file
            let max_bytes: usize = 5120;
            let file_size = meta.len() as usize;
            let read_size = std::cmp::min(file_size, max_bytes);
            let mut file = std::fs::File::open(&path)
                .map_err(|e| format!("Failed to open file: {}", e))?;
            let mut buf = vec![0u8; read_size];
            use std::io::Read;
            file.read_exact(&mut buf)
                .map_err(|e| format!("Failed to read file: {}", e))?;

            let truncated = file_size > max_bytes;
            let content = String::from_utf8_lossy(&buf).to_string();

            Ok(json!({
                "content": content,
                "size": file_size,
                "truncated": truncated,
            }))
        }

        "local_write" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            let content = get_str(&args, "content")?;
            validate_path(&path, "path")?;

            std::fs::write(&path, &content)
                .map_err(|e| format!("Failed to write file: {}", e))?;

            Ok(json!({ "success": true, "message": format!("Written {} bytes to {}", content.len(), path) }))
        }

        "local_mkdir" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;

            std::fs::create_dir_all(&path)
                .map_err(|e| format!("Failed to create directory: {}", e))?;

            Ok(json!({ "success": true, "message": format!("Created directory {}", path) }))
        }

        "local_delete" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;

            // Dangerous path protection (defense-in-depth)
            let home_dir = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_default();
            let normalized = path.trim_end_matches('/').trim_end_matches('\\');
            if normalized.is_empty() || normalized == "/" || normalized == "~" || normalized == "." || normalized == ".." || normalized == home_dir {
                return Err(format!("Refusing to delete dangerous path: {}", path));
            }

            let meta = std::fs::metadata(&path)
                .map_err(|e| format!("Path not found: {}", e))?;
            if meta.is_dir() {
                std::fs::remove_dir_all(&path)
                    .map_err(|e| format!("Failed to delete directory: {}", e))?;
            } else {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("Failed to delete file: {}", e))?;
            }

            Ok(json!({ "success": true, "message": format!("Deleted {}", path) }))
        }

        "local_rename" => {
            let base = context_local_path.as_deref();
            let from = resolve_local_path(&get_str(&args, "from")?, base);
            let to = resolve_local_path(&get_str(&args, "to")?, base);
            validate_path(&from, "from")?;
            validate_path(&to, "to")?;

            std::fs::rename(&from, &to)
                .map_err(|e| format!("Failed to rename: {}", e))?;

            Ok(json!({ "success": true, "message": format!("Renamed {} to {}", from, to) }))
        }

        "local_move_files" => {
            let base = context_local_path.as_deref();
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| resolve_local_path(s, base))).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let destination = resolve_local_path(&get_str(&args, "destination")?, base);
            validate_path(&destination, "destination")?;

            if paths.is_empty() {
                return Err("'paths' array is empty".to_string());
            }

            // Ensure destination directory exists
            std::fs::create_dir_all(&destination)
                .map_err(|e| format!("Failed to create destination directory: {}", e))?;

            let mut moved = Vec::new();
            let mut errors = Vec::new();
            let total = paths.len();

            for (idx, source) in paths.iter().enumerate() {
                if let Err(e) = validate_path(source, "path") {
                    errors.push(json!({ "file": source, "error": e }));
                    continue;
                }
                let filename = std::path::Path::new(source)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let dest_path = format!("{}/{}", destination.trim_end_matches('/'), filename);

                emit_tool_progress(&app, "local_move_files", idx as u32 + 1, total as u32, &filename);

                // Try rename first (fast, same-device move)
                match std::fs::rename(source, &dest_path) {
                    Ok(_) => moved.push(filename),
                    Err(_) => {
                        // Cross-device fallback: copy + delete
                        match std::fs::copy(source, &dest_path)
                            .and_then(|_| std::fs::remove_file(source))
                        {
                            Ok(_) => moved.push(filename),
                            Err(e) => errors.push(json!({ "file": filename, "error": e.to_string() })),
                        }
                    }
                }
            }

            Ok(json!({
                "moved": moved.len(),
                "failed": errors.len(),
                "total": total,
                "files": moved,
                "errors": errors,
            }))
        }

        "local_batch_rename" => {
            let base = context_local_path.as_deref();
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| resolve_local_path(s, base))).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let mode = get_str(&args, "mode")?;

            if paths.is_empty() {
                return Err("'paths' array is empty".to_string());
            }

            // Helper: split name and extension (preserve extension for files)
            fn split_name_ext(name: &str, is_dir: bool) -> (&str, &str) {
                if is_dir { return (name, ""); }
                match name.rfind('.') {
                    Some(pos) if pos > 0 => (&name[..pos], &name[pos..]),
                    _ => (name, ""),
                }
            }

            // Compute new names
            let mut renames: Vec<(String, String)> = Vec::new();
            let mut errors = Vec::new();

            for (idx, source) in paths.iter().enumerate() {
                if let Err(e) = validate_path(source, "path") {
                    errors.push(json!({ "file": source, "error": e }));
                    continue;
                }
                let src_path = std::path::Path::new(source);
                let filename = src_path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let is_dir = src_path.is_dir();
                let (name_no_ext, ext) = split_name_ext(&filename, is_dir);

                let new_name = match mode.as_str() {
                    "find_replace" => {
                        let find = get_str_opt(&args, "find").unwrap_or_default();
                        let replace_with = get_str_opt(&args, "replace").unwrap_or_default();
                        let case_sensitive = args.get("case_sensitive").and_then(|v| v.as_bool()).unwrap_or(false);
                        if find.is_empty() {
                            filename.clone()
                        } else if case_sensitive {
                            filename.replace(&find, &replace_with)
                        } else {
                            // Case-insensitive replace
                            let lower_find = find.to_lowercase();
                            let lower_name = filename.to_lowercase();
                            let mut result = String::new();
                            let mut start = 0;
                            while let Some(pos) = lower_name[start..].find(&lower_find) {
                                result.push_str(&filename[start..start + pos]);
                                result.push_str(&replace_with);
                                start += pos + find.len();
                            }
                            result.push_str(&filename[start..]);
                            result
                        }
                    }
                    "add_prefix" => {
                        let prefix = get_str_opt(&args, "prefix").unwrap_or_default();
                        format!("{}{}", prefix, filename)
                    }
                    "add_suffix" => {
                        let suffix = get_str_opt(&args, "suffix").unwrap_or_default();
                        format!("{}{}{}", name_no_ext, suffix, ext)
                    }
                    "sequential" => {
                        let base_name = get_str_opt(&args, "base_name").unwrap_or_else(|| "file".to_string());
                        let start_number = args.get("start_number").and_then(|v| v.as_u64()).unwrap_or(1);
                        let padding = args.get("padding").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
                        let num = start_number + idx as u64;
                        format!("{}_{:0>width$}{}", base_name, num, ext, width = padding)
                    }
                    _ => {
                        errors.push(json!({ "file": filename, "error": format!("Unknown rename mode: {}", mode) }));
                        continue;
                    }
                };

                if new_name != filename && !new_name.trim().is_empty() {
                    let parent = src_path.parent().unwrap_or(std::path::Path::new("/"));
                    let dest = parent.join(&new_name).to_string_lossy().to_string();
                    renames.push((source.clone(), dest));
                }
            }

            // Conflict detection
            let new_names: Vec<&str> = renames.iter().map(|(_, d)| d.as_str()).collect();
            let mut seen = std::collections::HashSet::new();
            for name in &new_names {
                if !seen.insert(*name) {
                    return Err(format!("Naming conflict detected: multiple files would be renamed to '{}'", name));
                }
            }

            // Execute renames
            let mut renamed = Vec::new();
            let total = renames.len();
            for (idx, (from, to)) in renames.iter().enumerate() {
                let filename = std::path::Path::new(from).file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                emit_tool_progress(&app, "local_batch_rename", idx as u32 + 1, total as u32, &filename);
                match std::fs::rename(from, to) {
                    Ok(_) => renamed.push(json!({ "from": from, "to": to })),
                    Err(e) => errors.push(json!({ "file": from, "error": e.to_string() })),
                }
            }

            Ok(json!({
                "renamed": renamed.len(),
                "failed": errors.len(),
                "total": paths.len(),
                "renames": renamed,
                "errors": errors,
            }))
        }

        "local_copy_files" => {
            let base = context_local_path.as_deref();
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| resolve_local_path(s, base))).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let destination = resolve_local_path(&get_str(&args, "destination")?, base);
            validate_path(&destination, "destination")?;

            if paths.is_empty() {
                return Err("'paths' array is empty".to_string());
            }

            std::fs::create_dir_all(&destination)
                .map_err(|e| format!("Failed to create destination directory: {}", e))?;

            fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<u64, String> {
                std::fs::create_dir_all(dst)
                    .map_err(|e| format!("Failed to create dir {}: {}", dst.display(), e))?;
                let mut count = 0u64;
                for entry in std::fs::read_dir(src)
                    .map_err(|e| format!("Failed to read dir {}: {}", src.display(), e))?
                {
                    let entry = entry.map_err(|e| e.to_string())?;
                    let src_path = entry.path();
                    let dst_path = dst.join(entry.file_name());
                    if src_path.is_dir() {
                        count += copy_dir_recursive(&src_path, &dst_path)?;
                    } else {
                        std::fs::copy(&src_path, &dst_path)
                            .map_err(|e| format!("Failed to copy {}: {}", src_path.display(), e))?;
                        count += 1;
                    }
                }
                Ok(count)
            }

            let mut copied = Vec::new();
            let mut errors = Vec::new();
            let total = paths.len();

            for (idx, source) in paths.iter().enumerate() {
                if let Err(e) = validate_path(source, "path") {
                    errors.push(json!({ "file": source, "error": e }));
                    continue;
                }
                let src_path = std::path::Path::new(source);
                let filename = src_path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let dest_path = format!("{}/{}", destination.trim_end_matches('/'), filename);

                emit_tool_progress(&app, "local_copy_files", idx as u32 + 1, total as u32, &filename);

                if src_path.is_dir() {
                    match copy_dir_recursive(src_path, std::path::Path::new(&dest_path)) {
                        Ok(_) => copied.push(filename),
                        Err(e) => errors.push(json!({ "file": filename, "error": e })),
                    }
                } else {
                    match std::fs::copy(source, &dest_path) {
                        Ok(_) => copied.push(filename),
                        Err(e) => errors.push(json!({ "file": filename, "error": e.to_string() })),
                    }
                }
            }

            Ok(json!({
                "copied": copied.len(),
                "failed": errors.len(),
                "total": total,
                "files": copied,
                "errors": errors,
            }))
        }

        "local_trash" => {
            let base = context_local_path.as_deref();
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| resolve_local_path(s, base))).collect())
                .ok_or("Missing 'paths' array parameter")?;

            if paths.is_empty() {
                return Err("'paths' array is empty".to_string());
            }

            let mut trashed = Vec::new();
            let mut errors = Vec::new();
            let total = paths.len();

            for (idx, path) in paths.iter().enumerate() {
                if let Err(e) = validate_path(path, "path") {
                    errors.push(json!({ "file": path, "error": e }));
                    continue;
                }
                let filename = std::path::Path::new(path).file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.clone());

                emit_tool_progress(&app, "local_trash", idx as u32 + 1, total as u32, &filename);

                match trash::delete(path) {
                    Ok(_) => trashed.push(filename),
                    Err(e) => errors.push(json!({ "file": filename, "error": e.to_string() })),
                }
            }

            Ok(json!({
                "trashed": trashed.len(),
                "failed": errors.len(),
                "total": total,
                "files": trashed,
                "errors": errors,
            }))
        }

        "local_file_info" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;

            let p = std::path::Path::new(&path);
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| format!("Failed to stat: {}", e))?;

            let mut info = json!({
                "path": path,
                "name": p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                "size": meta.len(),
                "is_file": meta.is_file(),
                "is_dir": meta.is_dir(),
                "is_symlink": meta.is_symlink(),
                "readonly": meta.permissions().readonly(),
            });

            // Timestamps
            if let Ok(modified) = meta.modified() {
                if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                    info["modified"] = json!(dur.as_secs());
                }
            }
            if let Ok(created) = meta.created() {
                if let Ok(dur) = created.duration_since(std::time::UNIX_EPOCH) {
                    info["created"] = json!(dur.as_secs());
                }
            }

            // Unix-specific: permissions octal, uid, gid
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                info["permissions_octal"] = json!(format!("{:o}", meta.mode()));
                info["uid"] = json!(meta.uid());
                info["gid"] = json!(meta.gid());
            }

            // MIME type from extension
            if meta.is_file() {
                if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    let mime = match ext.to_lowercase().as_str() {
                        "pdf" => "application/pdf", "txt" => "text/plain",
                        "html" | "htm" => "text/html", "css" => "text/css",
                        "js" => "text/javascript", "json" => "application/json",
                        "xml" => "application/xml", "zip" => "application/zip",
                        "7z" => "application/x-7z-compressed", "tar" => "application/x-tar",
                        "gz" => "application/gzip", "png" => "image/png",
                        "jpg" | "jpeg" => "image/jpeg", "gif" => "image/gif",
                        "svg" => "image/svg+xml", "mp3" => "audio/mpeg",
                        "mp4" => "video/mp4", "rs" => "text/x-rust",
                        "ts" | "tsx" => "text/typescript", "py" => "text/x-python",
                        _ => "application/octet-stream",
                    };
                    info["mime_type"] = json!(mime);
                }
            }

            Ok(info)
        }

        "local_disk_usage" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;

            let p = std::path::Path::new(&path);
            if !p.is_dir() {
                return Err(format!("Path is not a directory: {}", path));
            }

            // Inline calculation (same logic as filesystem.rs calculate_folder_size)
            let mut total_bytes: u64 = 0;
            let mut file_count: u64 = 0;
            let mut dir_count: u64 = 0;
            const MAX_ENTRIES: u64 = 500_000;
            let mut entry_count: u64 = 0;

            for entry in walkdir::WalkDir::new(&path)
                .follow_links(false)
                .max_depth(100)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                entry_count += 1;
                if entry_count > MAX_ENTRIES { break; }
                if entry.file_type().is_file() {
                    total_bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                    file_count += 1;
                } else if entry.file_type().is_dir() && entry.path() != p {
                    dir_count += 1;
                }
            }

            Ok(json!({
                "path": path,
                "total_bytes": total_bytes,
                "total_human": format!("{:.1} MB", total_bytes as f64 / 1_048_576.0),
                "file_count": file_count,
                "dir_count": dir_count,
            }))
        }

        "local_find_duplicates" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;
            let min_size = args.get("min_size").and_then(|v| v.as_u64()).unwrap_or(1024);

            let p = std::path::Path::new(&path);
            if !p.is_dir() {
                return Err(format!("Path is not a directory: {}", path));
            }

            // Phase 1: group files by size
            let mut size_groups: std::collections::HashMap<u64, Vec<std::path::PathBuf>> = std::collections::HashMap::new();
            const MAX_SCAN: u64 = 50_000;
            let mut scan_count: u64 = 0;

            for entry in walkdir::WalkDir::new(&path)
                .follow_links(false)
                .max_depth(50)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() { continue; }
                scan_count += 1;
                if scan_count > MAX_SCAN { break; }
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if size < min_size { continue; }
                size_groups.entry(size).or_default().push(entry.into_path());
            }

            // Phase 2: hash files with matching sizes
            use md5::{Md5, Digest};
            use std::io::Read;
            let mut hash_groups: std::collections::HashMap<String, (u64, Vec<String>)> = std::collections::HashMap::new();

            for (size, files) in &size_groups {
                if files.len() < 2 { continue; }
                for file_path in files {
                    if let Ok(mut f) = std::fs::File::open(file_path) {
                        let mut hasher = Md5::new();
                        let mut buf = [0u8; 8192];
                        loop {
                            match f.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => hasher.update(&buf[..n]),
                                Err(_) => break,
                            }
                        }
                        let hash = format!("{:x}", hasher.finalize());
                        let entry = hash_groups.entry(hash).or_insert_with(|| (*size, Vec::new()));
                        entry.1.push(file_path.to_string_lossy().to_string());
                    }
                }
            }

            // Phase 3: collect duplicates
            let mut duplicates: Vec<Value> = hash_groups
                .into_iter()
                .filter(|(_, (_, files))| files.len() >= 2)
                .map(|(hash, (size, files))| json!({
                    "hash": hash,
                    "size": size,
                    "count": files.len(),
                    "wasted_bytes": size * (files.len() as u64 - 1),
                    "files": files,
                }))
                .collect();

            duplicates.sort_by(|a, b| {
                let wa = a["wasted_bytes"].as_u64().unwrap_or(0);
                let wb = b["wasted_bytes"].as_u64().unwrap_or(0);
                wb.cmp(&wa)
            });

            let total_wasted: u64 = duplicates.iter()
                .map(|d| d["wasted_bytes"].as_u64().unwrap_or(0))
                .sum();

            Ok(json!({
                "groups": duplicates.len(),
                "total_wasted_bytes": total_wasted,
                "total_wasted_human": format!("{:.1} MB", total_wasted as f64 / 1_048_576.0),
                "duplicates": duplicates,
            }))
        }

        "local_edit" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            let find = get_str(&args, "find")?;
            let replace = get_str(&args, "replace")?;
            let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(true);
            validate_path(&path, "path")?;

            let mut content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;

            // Strip UTF-8 BOM if present (common in Windows-created files)
            if content.starts_with('\u{FEFF}') {
                content = content.strip_prefix('\u{FEFF}').unwrap().to_string();
            }

            let occurrences = content.matches(&find).count();
            if occurrences == 0 {
                return Ok(json!({
                    "success": false,
                    "message": "String not found in file",
                    "occurrences": 0,
                }));
            }

            let new_content = if replace_all {
                content.replace(&find, &replace)
            } else {
                content.replacen(&find, &replace, 1)
            };

            std::fs::write(&path, &new_content)
                .map_err(|e| format!("Failed to write file: {}", e))?;

            let replaced = if replace_all { occurrences } else { 1 };
            Ok(json!({
                "success": true,
                "message": format!("Replaced {} occurrence(s) in {}", replaced, path),
                "occurrences": occurrences,
                "replaced": replaced,
            }))
        }

        "remote_edit" => {
            let path = get_str(&args, "path")?;
            let find = get_str(&args, "find")?;
            let replace = get_str(&args, "replace")?;
            let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(true);
            validate_path(&path, "path")?;

            // Download file content
            let bytes = download_from_provider(&state, &app_state, &path).await?;

            let mut content = String::from_utf8(bytes)
                .map_err(|_| "File is not valid UTF-8 text".to_string())?;

            // Strip UTF-8 BOM if present
            content = content.strip_prefix('\u{FEFF}').unwrap_or(&content).to_string();

            let occurrences = content.matches(&find).count();
            if occurrences == 0 {
                return Ok(json!({
                    "success": false,
                    "message": "String not found in file",
                    "occurrences": 0,
                }));
            }

            let new_content = if replace_all {
                content.replace(&find, &replace)
            } else {
                content.replacen(&find, &replace, 1)
            };

            // Write back via temp file + upload
            let tmp_path = std::env::temp_dir()
                .join(format!("aeroftp_{}", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .to_string();
            std::fs::write(&tmp_path, &new_content)
                .map_err(|e| format!("Failed to write temp file: {}", e))?;

            let upload_result = if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                provider.upload(&tmp_path, &path, None).await.map_err(|e| e.to_string())
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.upload_file(&tmp_path, &path).await.map_err(|e| e.to_string())
            } else {
                Err("Not connected".to_string())
            };

            let _ = std::fs::remove_file(&tmp_path);
            upload_result?;

            let replaced = if replace_all { occurrences } else { 1 };
            Ok(json!({
                "success": true,
                "message": format!("Replaced {} occurrence(s) in {}", replaced, path),
                "occurrences": occurrences,
                "replaced": replaced,
            }))
        }

        "upload_files" => {
            let local_paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let remote_dir = get_str(&args, "remote_dir")?;
            validate_path(&remote_dir, "remote_dir")?;

            let mut uploaded = Vec::new();
            let mut errors = Vec::new();
            let total = local_paths.len();

            for (idx, local_path) in local_paths.iter().enumerate() {
                validate_path(local_path, "path").map_err(|e| e.to_string())?;
                let filename = std::path::Path::new(local_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let remote_path = format!("{}/{}", remote_dir.trim_end_matches('/'), filename);

                emit_tool_progress(&app, "upload_files", idx as u32 + 1, total as u32, &filename);

                let result = if has_provider(&state).await {
                    let mut provider = state.provider.lock().await;
                    let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                    provider.upload(local_path, &remote_path, None).await.map_err(|e| e.to_string())
                } else if has_ftp(&app_state).await {
                    let mut manager = app_state.ftp_manager.lock().await;
                    manager.upload_file(local_path, &remote_path).await.map_err(|e| e.to_string())
                } else {
                    Err("Not connected to any server".to_string())
                };

                match result {
                    Ok(_) => uploaded.push(filename),
                    Err(e) => errors.push(json!({ "file": filename, "error": e })),
                }
            }

            Ok(json!({
                "uploaded": uploaded.len(),
                "failed": errors.len(),
                "files": uploaded,
                "errors": errors,
            }))
        }

        "download_files" => {
            let remote_paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let local_dir = get_str(&args, "local_dir")?;
            validate_path(&local_dir, "local_dir")?;

            // Ensure local dir exists
            std::fs::create_dir_all(&local_dir)
                .map_err(|e| format!("Failed to create local directory: {}", e))?;

            let mut downloaded = Vec::new();
            let mut errors = Vec::new();
            let total = remote_paths.len();

            for (idx, remote_path) in remote_paths.iter().enumerate() {
                validate_path(remote_path, "path").map_err(|e| e.to_string())?;
                let filename = std::path::Path::new(remote_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let local_path = format!("{}/{}", local_dir.trim_end_matches('/'), filename);

                emit_tool_progress(&app, "download_files", idx as u32 + 1, total as u32, &filename);

                let result = if has_provider(&state).await {
                    let mut provider = state.provider.lock().await;
                    let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                    provider.download(remote_path, &local_path, None).await.map_err(|e| e.to_string())
                } else if has_ftp(&app_state).await {
                    let mut manager = app_state.ftp_manager.lock().await;
                    manager.download_file(remote_path, &local_path).await.map_err(|e| e.to_string())
                } else {
                    Err("Not connected to any server".to_string())
                };

                match result {
                    Ok(_) => downloaded.push(filename),
                    Err(e) => errors.push(json!({ "file": filename, "error": e })),
                }
            }

            Ok(json!({
                "downloaded": downloaded.len(),
                "failed": errors.len(),
                "files": downloaded,
                "errors": errors,
            }))
        }

        "sync_preview" => {
            let local_path = get_str(&args, "local_path")?;
            let remote_path = get_str(&args, "remote_path")?;
            validate_path(&local_path, "local_path")?;
            validate_path(&remote_path, "remote_path")?;

            // Collect local files
            let local_files: std::collections::HashMap<String, u64> = std::fs::read_dir(&local_path)
                .map_err(|e| format!("Failed to read local directory: {}", e))?
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let meta = e.metadata().ok()?;
                    if meta.is_file() {
                        Some((e.file_name().to_string_lossy().to_string(), meta.len()))
                    } else {
                        None
                    }
                })
                .collect();

            // Collect remote files
            let remote_files: std::collections::HashMap<String, u64> = if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                let entries = provider.list(&remote_path).await.map_err(|e| e.to_string())?;
                entries.iter().filter(|e| !e.is_dir).map(|e| (e.name.clone(), e.size)).collect()
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.change_dir(&remote_path).await.map_err(|e| e.to_string())?;
                let files = manager.list_files().await.map_err(|e| e.to_string())?;
                files.iter().filter(|f| !f.is_dir).map(|f| (f.name.clone(), f.size.unwrap_or(0))).collect()
            } else {
                return Err("Not connected to any server".to_string());
            };

            // Compare
            let mut only_local: Vec<Value> = Vec::new();
            let mut only_remote: Vec<Value> = Vec::new();
            let mut size_diff: Vec<Value> = Vec::new();
            let mut identical: Vec<String> = Vec::new();

            for (name, local_size) in &local_files {
                match remote_files.get(name) {
                    Some(&remote_size) if *local_size == remote_size => {
                        identical.push(name.clone());
                    }
                    Some(&remote_size) => {
                        size_diff.push(json!({
                            "name": name,
                            "local_size": local_size,
                            "remote_size": remote_size,
                        }));
                    }
                    None => {
                        only_local.push(json!({ "name": name, "size": local_size }));
                    }
                }
            }
            for (name, remote_size) in &remote_files {
                if !local_files.contains_key(name) {
                    only_remote.push(json!({ "name": name, "size": remote_size }));
                }
            }

            Ok(json!({
                "local_path": local_path,
                "remote_path": remote_path,
                "local_files": local_files.len(),
                "remote_files": remote_files.len(),
                "identical": identical.len(),
                "only_local": only_local,
                "only_remote": only_remote,
                "size_different": size_diff,
                "synced": only_local.is_empty() && only_remote.is_empty() && size_diff.is_empty(),
            }))
        }

        "archive_compress" => {
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let output_path = get_str(&args, "output_path")?;
            let format = get_str_opt(&args, "format").unwrap_or_else(|| "zip".to_string());
            let password = get_str_opt(&args, "password");
            let compression_level = args.get("compression_level").and_then(|v| v.as_i64());
            validate_path(&output_path, "output_path")?;
            for p in &paths {
                validate_path(p, "path")?;
            }

            // Delegate to existing Tauri compress commands
            let result = match format.as_str() {
                "zip" => {
                    crate::compress_files_core(paths, output_path.clone(), password, compression_level).await
                }
                "7z" => {
                    crate::compress_7z_core(paths, output_path.clone(), password, compression_level).await
                }
                "tar" | "tar.gz" | "tar.bz2" | "tar.xz" => {
                    crate::compress_tar_core(paths, output_path.clone(), format.clone(), compression_level).await
                }
                _ => Err(format!("Unsupported format: {}. Use zip, 7z, tar, tar.gz, tar.bz2, or tar.xz", format)),
            };

            match result {
                Ok(msg) => Ok(json!({
                    "success": true,
                    "message": msg,
                    "output_path": output_path,
                    "format": format,
                })),
                Err(e) => Err(e),
            }
        }

        "archive_decompress" => {
            let archive_path = get_str(&args, "archive_path")?;
            let output_dir = get_str(&args, "output_dir")?;
            let password = get_str_opt(&args, "password");
            let create_subfolder = args.get("create_subfolder").and_then(|v| v.as_bool()).unwrap_or(true);
            validate_path(&archive_path, "archive_path")?;
            validate_path(&output_dir, "output_dir")?;

            // Detect format from extension
            let lower = archive_path.to_lowercase();
            let result = if lower.ends_with(".zip") {
                crate::extract_archive_core(archive_path.clone(), output_dir.clone(), create_subfolder, password).await
            } else if lower.ends_with(".7z") {
                crate::extract_7z_core(archive_path.clone(), output_dir.clone(), password, create_subfolder).await
            } else if lower.ends_with(".tar") || lower.ends_with(".tar.gz") || lower.ends_with(".tgz")
                || lower.ends_with(".tar.bz2") || lower.ends_with(".tar.xz") {
                crate::extract_tar_core(archive_path.clone(), output_dir.clone(), create_subfolder).await
            } else {
                Err(format!("Unsupported archive format: {}", archive_path))
            };

            match result {
                Ok(msg) => Ok(json!({
                    "success": true,
                    "message": msg,
                    "archive_path": archive_path,
                    "output_dir": output_dir,
                })),
                Err(e) => Err(e),
            }
        }

        "hash_file" => {
            let path = get_str(&args, "path")?;
            let algorithm = get_str_opt(&args, "algorithm").unwrap_or_else(|| "sha256".to_string());
            validate_path(&path, "path")?;

            let p = std::path::Path::new(&path);
            if !p.is_file() {
                return Err(format!("Path is not a file: {}", path));
            }

            // Delegate to existing cyber_tools::hash_file
            let hash = crate::cyber_tools::hash_file(path.clone(), algorithm.clone()).await?;

            Ok(json!({
                "path": path,
                "algorithm": algorithm,
                "hash": hash,
            }))
        }

        "rag_index" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;
            let recursive = args.get("recursive").and_then(|v| v.as_bool()).unwrap_or(true);
            let max_files = args.get("max_files").and_then(|v| v.as_u64()).unwrap_or(200) as u32;

            const TEXT_EXTENSIONS: &[&str] = &[
                "rs", "ts", "tsx", "js", "jsx", "py", "json", "toml", "yaml", "yml",
                "md", "txt", "html", "css", "sh", "sql", "xml", "csv", "env", "cfg",
                "ini", "conf", "log", "go", "java", "c", "cpp", "h", "hpp", "rb",
                "php", "swift", "kt",
            ];

            fn is_text_file(path: &std::path::Path) -> bool {
                path.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| TEXT_EXTENSIONS.contains(&e.to_lowercase().as_str()))
                    .unwrap_or(false)
            }

            fn scan_dir(
                dir: &std::path::Path,
                base: &std::path::Path,
                recursive: bool,
                files: &mut Vec<Value>,
                dirs_count: &mut u32,
                max_files: u32,
            ) {
                let entries = match std::fs::read_dir(dir) {
                    Ok(e) => e,
                    Err(_) => return,
                };
                for entry in entries.flatten() {
                    if files.len() >= max_files as usize {
                        return;
                    }
                    let entry_path = entry.path();
                    let meta = match entry.metadata() {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    if meta.is_dir() {
                        *dirs_count += 1;
                        if recursive {
                            scan_dir(&entry_path, base, recursive, files, dirs_count, max_files);
                        }
                    } else if meta.is_file() {
                        let rel = entry_path.strip_prefix(base)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| entry_path.to_string_lossy().to_string());
                        let name = entry_path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let ext = entry_path.extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let size = meta.len();

                        let preview = if is_text_file(&entry_path) && size < 50_000 {
                            std::fs::read_to_string(&entry_path)
                                .ok()
                                .map(|content| {
                                    content.lines().take(20).collect::<Vec<_>>().join("\n")
                                })
                        } else {
                            None
                        };

                        let mut file_obj = json!({
                            "name": name,
                            "path": rel,
                            "size": size,
                            "ext": ext,
                        });
                        if let Some(p) = preview {
                            file_obj.as_object_mut().unwrap().insert("preview".to_string(), json!(p));
                        }
                        files.push(file_obj);
                    }
                }
            }

            let base_path = std::path::Path::new(&path);
            if !base_path.is_dir() {
                return Err(format!("Not a directory: {}", path));
            }

            let mut files: Vec<Value> = Vec::new();
            let mut dirs_count: u32 = 0;
            scan_dir(base_path, base_path, recursive, &mut files, &mut dirs_count, max_files);

            // Emit progress after scan completes
            emit_tool_progress(&app, "rag_index", files.len() as u32, files.len() as u32, "scan complete");

            let total_size: u64 = files.iter()
                .filter_map(|f| f.get("size").and_then(|s| s.as_u64()))
                .sum();

            let mut extensions: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
            for f in &files {
                if let Some(ext) = f.get("ext").and_then(|e| e.as_str()) {
                    if !ext.is_empty() {
                        *extensions.entry(ext.to_string()).or_insert(0) += 1;
                    }
                }
            }

            Ok(json!({
                "files_count": files.len(),
                "dirs_count": dirs_count,
                "total_size": total_size,
                "extensions": extensions,
                "files": files,
            }))
        }

        "rag_search" => {
            let query = get_str(&args, "query")?;
            let path = get_str_opt(&args, "path").unwrap_or_else(|| ".".to_string());
            validate_path(&path, "path")?;
            let max_results = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

            const SEARCH_EXTENSIONS: &[&str] = &[
                "rs", "ts", "tsx", "js", "jsx", "py", "json", "toml", "yaml", "yml",
                "md", "txt", "html", "css", "sh", "sql", "xml", "csv", "env", "cfg",
                "ini", "conf", "log", "go", "java", "c", "cpp", "h", "hpp", "rb",
                "php", "swift", "kt",
            ];

            fn is_searchable(path: &std::path::Path) -> bool {
                path.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| SEARCH_EXTENSIONS.contains(&e.to_lowercase().as_str()))
                    .unwrap_or(false)
            }

            fn search_dir(
                dir: &std::path::Path,
                base: &std::path::Path,
                query_lower: &str,
                matches: &mut Vec<Value>,
                files_scanned: &mut u32,
                max_results: usize,
                max_files: u32,
            ) {
                let entries = match std::fs::read_dir(dir) {
                    Ok(e) => e,
                    Err(_) => return,
                };
                for entry in entries.flatten() {
                    if matches.len() >= max_results || *files_scanned >= max_files {
                        return;
                    }
                    let entry_path = entry.path();
                    let meta = match entry.metadata() {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    if meta.is_dir() {
                        search_dir(&entry_path, base, query_lower, matches, files_scanned, max_results, max_files);
                    } else if meta.is_file() && is_searchable(&entry_path) && meta.len() < 100_000 {
                        *files_scanned += 1;
                        let rel = entry_path.strip_prefix(base)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| entry_path.to_string_lossy().to_string());

                        if let Ok(content) = std::fs::read_to_string(&entry_path) {
                            for (line_num, line) in content.lines().enumerate() {
                                if matches.len() >= max_results {
                                    break;
                                }
                                if line.to_lowercase().contains(query_lower) {
                                    matches.push(json!({
                                        "path": rel,
                                        "line": line_num + 1,
                                        "context": line.chars().take(200).collect::<String>(),
                                    }));
                                }
                            }
                        }
                    }
                }
            }

            let base_path = std::path::Path::new(&path);
            if !base_path.is_dir() {
                return Err(format!("Not a directory: {}", path));
            }

            let query_lower = query.to_lowercase();
            let mut matches: Vec<Value> = Vec::new();
            let mut files_scanned: u32 = 0;
            search_dir(base_path, base_path, &query_lower, &mut matches, &mut files_scanned, max_results, 500);

            Ok(json!({
                "query": query,
                "files_scanned": files_scanned,
                "matches": matches,
            }))
        }

        "preview_edit" => {
            let path = get_str(&args, "path")?;
            let find = get_str(&args, "find")?;
            let replace = get_str(&args, "replace")?;
            let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(true);
            let remote = args.get("remote").and_then(|v| v.as_bool()).unwrap_or(false);
            validate_path(&path, "path")?;

            const MAX_PREVIEW_BYTES: usize = 100 * 1024; // 100KB

            let mut content = if remote {
                let bytes = download_from_provider(&state, &app_state, &path).await?;
                if bytes.len() > MAX_PREVIEW_BYTES {
                    return Ok(json!({
                        "success": false,
                        "message": "File too large for preview (max 100KB)",
                    }));
                }
                String::from_utf8(bytes)
                    .map_err(|_| "File is not valid UTF-8 text".to_string())?
            } else {
                let meta = std::fs::metadata(&path)
                    .map_err(|e| format!("Failed to stat file: {}", e))?;
                if meta.len() as usize > MAX_PREVIEW_BYTES {
                    return Ok(json!({
                        "success": false,
                        "message": "File too large for preview (max 100KB)",
                    }));
                }
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("Failed to read file: {}", e))?
            };

            // Strip UTF-8 BOM if present
            if content.starts_with('\u{FEFF}') {
                content = content.strip_prefix('\u{FEFF}').unwrap().to_string();
            }

            let occurrences = content.matches(&find).count();
            if occurrences == 0 {
                return Ok(json!({
                    "success": false,
                    "message": "String not found in file",
                    "occurrences": 0,
                }));
            }

            let modified_content = if replace_all {
                content.replace(&find, &replace)
            } else {
                content.replacen(&find, &replace, 1)
            };

            let replaced = if replace_all { occurrences } else { 1 };
            Ok(json!({
                "success": true,
                "original": content,
                "modified": modified_content,
                "occurrences": occurrences,
                "replaced": replaced,
            }))
        }

        "agent_memory_write" => {
            let entry = args.get("entry")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'entry' parameter")?;
            let category = args.get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("general");

            // FIX 12: Sanitize category — only alphanumeric, underscore, hyphen; max 30 chars
            let sanitized_category: String = category.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .take(30)
                .collect();

            // FIX 11: Require explicit project_path and validate it
            let project_path = args.get("project_path")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'project_path' parameter")?;
            validate_path(project_path, "project_path")?;

            let formatted = format!("\n[{}] [{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M"),
                sanitized_category,
                entry
            );

            crate::context_intelligence::write_agent_memory(
                project_path.to_string(),
                formatted
            ).await.map_err(|e| e.to_string())?;

            Ok(json!({
                "success": true,
                "message": format!("Memory entry saved: [{}] {}", sanitized_category, entry)
            }))
        }

        "local_grep" => {
            let path = get_str(&args, "path")?;
            let pattern = get_str(&args, "pattern")?;
            validate_path(&path, "path")?;

            let glob_filter = get_str_opt(&args, "glob");
            let max_results = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let context_lines = args.get("context_lines").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
            let case_sensitive = args.get("case_sensitive").and_then(|v| v.as_bool()).unwrap_or(true);

            let re = if case_sensitive {
                regex::Regex::new(&pattern)
            } else {
                regex::RegexBuilder::new(&pattern).case_insensitive(true).build()
            }.map_err(|e| format!("Invalid regex: {}", e))?;

            let base_path = std::path::Path::new(&path);
            if !base_path.is_dir() {
                return Err(format!("Not a directory: {}", path));
            }

            // Compile glob pattern if provided
            let glob_re = if let Some(ref g) = glob_filter {
                let glob_pattern = g.replace('.', "\\.").replace('*', ".*").replace('?', ".");
                regex::RegexBuilder::new(&format!("^{}$", glob_pattern))
                    .case_insensitive(true)
                    .build()
                    .ok()
            } else {
                None
            };

            let mut matches: Vec<Value> = Vec::new();
            let mut files_searched: u32 = 0;
            const MAX_FILE_SIZE: u64 = 10_485_760; // 10MB

            for entry in walkdir::WalkDir::new(&path)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if matches.len() >= max_results {
                    break;
                }
                let entry_path = entry.path();
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if !meta.is_file() || meta.len() > MAX_FILE_SIZE {
                    continue;
                }

                // Apply glob filter on filename
                if let Some(ref gre) = glob_re {
                    if let Some(name) = entry_path.file_name().and_then(|n| n.to_str()) {
                        if !gre.is_match(name) {
                            continue;
                        }
                    }
                }

                // Skip binary files: check first 8KB for null bytes
                let bytes = match std::fs::read(entry_path) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                let check_len = bytes.len().min(8192);
                if bytes[..check_len].contains(&0) {
                    continue;
                }

                let content = match String::from_utf8(bytes) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                files_searched += 1;

                let lines: Vec<&str> = content.lines().collect();
                for (i, line) in lines.iter().enumerate() {
                    if matches.len() >= max_results {
                        break;
                    }
                    if re.is_match(line) {
                        let rel = entry_path.strip_prefix(base_path)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| entry_path.to_string_lossy().to_string());

                        let ctx_before: Vec<&str> = lines[i.saturating_sub(context_lines)..i]
                            .to_vec();
                        let ctx_after: Vec<&str> = lines[(i + 1)..lines.len().min(i + 1 + context_lines)]
                            .to_vec();

                        matches.push(json!({
                            "file": rel,
                            "line_number": i + 1,
                            "line": line.chars().take(500).collect::<String>(),
                            "context_before": ctx_before,
                            "context_after": ctx_after,
                        }));
                    }
                }
            }

            Ok(json!({
                "success": true,
                "pattern": pattern,
                "total_matches": matches.len(),
                "files_searched": files_searched,
                "matches": matches,
            }))
        }

        "local_head" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;
            let num_lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(20).min(500) as usize;

            let p = std::path::Path::new(&path);
            if !p.is_file() {
                return Err(format!("Not a file: {}", path));
            }
            let meta = std::fs::metadata(&path).map_err(|e| format!("Failed to stat: {}", e))?;
            if meta.len() > 52_428_800 {
                return Err("File too large (max 50MB)".to_string());
            }

            use std::io::{BufRead, BufReader};
            let file = std::fs::File::open(&path).map_err(|e| format!("Failed to open: {}", e))?;
            let reader = BufReader::new(file);
            let mut result_lines: Vec<String> = Vec::new();
            let mut total_lines: usize = 0;

            for line in reader.lines() {
                total_lines += 1;
                if result_lines.len() < num_lines {
                    match line {
                        Ok(l) => result_lines.push(l),
                        Err(_) => result_lines.push("[binary data]".to_string()),
                    }
                } else {
                    // Keep counting total lines
                    if line.is_err() { continue; }
                }
            }

            let content = result_lines.join("\n");
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();

            Ok(json!({
                "success": true,
                "content": content,
                "lines_read": result_lines.len(),
                "total_lines": total_lines,
                "file_name": name,
            }))
        }

        "local_tail" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;
            let num_lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(20).min(500) as usize;

            let p = std::path::Path::new(&path);
            if !p.is_file() {
                return Err(format!("Not a file: {}", path));
            }
            let meta = std::fs::metadata(&path).map_err(|e| format!("Failed to stat: {}", e))?;
            if meta.len() > 52_428_800 {
                return Err("File too large (max 50MB)".to_string());
            }

            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read: {}", e))?;
            let all_lines: Vec<&str> = content.lines().collect();
            let total_lines = all_lines.len();
            let start = total_lines.saturating_sub(num_lines);
            let result_lines: Vec<&str> = all_lines[start..].to_vec();
            let result_content = result_lines.join("\n");
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();

            Ok(json!({
                "success": true,
                "content": result_content,
                "lines_read": result_lines.len(),
                "total_lines": total_lines,
                "file_name": name,
            }))
        }

        "local_stat_batch" => {
            let paths = args.get("paths")
                .and_then(|v| v.as_array())
                .ok_or("Missing 'paths' array")?;

            if paths.len() > 100 {
                return Err(format!("Too many paths: {} (max 100)", paths.len()));
            }

            let base = context_local_path.as_deref();
            let mut files: Vec<Value> = Vec::new();

            for p in paths.iter().filter_map(|v| v.as_str()) {
                let resolved = resolve_local_path(p, base);
                let path = std::path::Path::new(&resolved);
                if let Ok(meta) = std::fs::metadata(path) {
                    let size = meta.len();
                    let modified = meta.modified().ok().map(|t| {
                        let datetime: chrono::DateTime<chrono::Local> = t.into();
                        datetime.format("%Y-%m-%d %H:%M:%S").to_string()
                    });
                    let is_file = meta.is_file();
                    let is_dir = meta.is_dir();
                    #[cfg(unix)]
                    let permissions = {
                        use std::os::unix::fs::PermissionsExt;
                        format!("{:o}", meta.permissions().mode() & 0o777)
                    };
                    #[cfg(not(unix))]
                    let permissions = if meta.permissions().readonly() { "r--" } else { "rw-" }.to_string();

                    let size_human = if size < 1024 {
                        format!("{} B", size)
                    } else if size < 1_048_576 {
                        format!("{:.1} KB", size as f64 / 1024.0)
                    } else if size < 1_073_741_824 {
                        format!("{:.1} MB", size as f64 / 1_048_576.0)
                    } else {
                        format!("{:.2} GB", size as f64 / 1_073_741_824.0)
                    };

                    files.push(json!({
                        "path": resolved,
                        "name": path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                        "size": size,
                        "size_human": size_human,
                        "modified": modified,
                        "is_file": is_file,
                        "is_dir": is_dir,
                        "permissions": permissions,
                        "exists": true,
                    }));
                } else {
                    files.push(json!({
                        "path": resolved,
                        "name": path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                        "exists": false,
                    }));
                }
            }

            Ok(json!({
                "success": true,
                "files": files,
                "total": files.len(),
            }))
        }

        "local_diff" => {
            let path_a = get_str(&args, "path_a")?;
            let path_b = get_str(&args, "path_b")?;
            validate_path(&path_a, "path_a")?;
            validate_path(&path_b, "path_b")?;
            let context_lines = args.get("context_lines").and_then(|v| v.as_u64()).unwrap_or(3) as usize;

            const MAX_DIFF_SIZE: u64 = 5_242_880; // 5MB

            let meta_a = std::fs::metadata(&path_a).map_err(|e| format!("path_a: {}", e))?;
            let meta_b = std::fs::metadata(&path_b).map_err(|e| format!("path_b: {}", e))?;

            if !meta_a.is_file() {
                return Err(format!("Not a file: {}", path_a));
            }
            if !meta_b.is_file() {
                return Err(format!("Not a file: {}", path_b));
            }
            if meta_a.len() > MAX_DIFF_SIZE {
                return Err(format!("File A too large: {:.1} MB (max 5MB)", meta_a.len() as f64 / 1_048_576.0));
            }
            if meta_b.len() > MAX_DIFF_SIZE {
                return Err(format!("File B too large: {:.1} MB (max 5MB)", meta_b.len() as f64 / 1_048_576.0));
            }

            let content_a = std::fs::read_to_string(&path_a)
                .map_err(|e| format!("Failed to read file A: {}", e))?;
            let content_b = std::fs::read_to_string(&path_b)
                .map_err(|e| format!("Failed to read file B: {}", e))?;

            let diff = similar::TextDiff::from_lines(&content_a, &content_b);
            let unified = diff.unified_diff()
                .context_radius(context_lines)
                .header(&path_a, &path_b)
                .to_string();

            let mut additions: usize = 0;
            let mut deletions: usize = 0;
            for change in diff.iter_all_changes() {
                match change.tag() {
                    similar::ChangeTag::Insert => additions += 1,
                    similar::ChangeTag::Delete => deletions += 1,
                    _ => {}
                }
            }

            let name_a = std::path::Path::new(&path_a).file_name()
                .map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let name_b = std::path::Path::new(&path_b).file_name()
                .map(|n| n.to_string_lossy().to_string()).unwrap_or_default();

            Ok(json!({
                "success": true,
                "diff": unified,
                "identical": additions == 0 && deletions == 0,
                "stats": {
                    "additions": additions,
                    "deletions": deletions,
                    "file_a": name_a,
                    "file_b": name_b,
                },
            }))
        }

        "local_tree" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;
            let max_depth = args.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(3).min(10) as usize;
            let show_hidden = args.get("show_hidden").and_then(|v| v.as_bool()).unwrap_or(false);
            let glob_filter = get_str_opt(&args, "glob");

            let base_path = std::path::Path::new(&path);
            if !base_path.is_dir() {
                return Err(format!("Not a directory: {}", path));
            }

            // Compile glob pattern if provided
            let glob_re = if let Some(ref g) = glob_filter {
                let glob_pattern = g.replace('.', "\\.").replace('*', ".*").replace('?', ".");
                regex::RegexBuilder::new(&format!("^{}$", glob_pattern))
                    .case_insensitive(true)
                    .build()
                    .ok()
            } else {
                None
            };

            const MAX_ENTRIES: usize = 1000;
            let mut tree_lines: Vec<String> = Vec::new();
            let mut file_count: u32 = 0;
            let mut dir_count: u32 = 0;
            let mut total_size: u64 = 0;

            let root_name = base_path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            tree_lines.push(format!("{}/", root_name));

            #[allow(clippy::too_many_arguments)]
            fn build_tree(
                dir: &std::path::Path,
                prefix: &str,
                depth: usize,
                max_depth: usize,
                show_hidden: bool,
                glob_re: &Option<regex::Regex>,
                lines: &mut Vec<String>,
                file_count: &mut u32,
                dir_count: &mut u32,
                total_size: &mut u64,
                max_entries: usize,
            ) {
                if depth >= max_depth || lines.len() >= max_entries {
                    return;
                }
                let mut entries: Vec<_> = match std::fs::read_dir(dir) {
                    Ok(e) => e.filter_map(|e| e.ok()).collect(),
                    Err(_) => return,
                };
                entries.sort_by_key(|e| {
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    (!is_dir, e.file_name().to_string_lossy().to_lowercase())
                });

                let count = entries.len();
                for (i, entry) in entries.iter().enumerate() {
                    if lines.len() >= max_entries {
                        lines.push(format!("{}... (truncated at {} entries)", prefix, max_entries));
                        return;
                    }
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !show_hidden && name.starts_with('.') {
                        continue;
                    }
                    let is_last = i == count - 1;
                    let connector = if is_last { "\u{2514}\u{2500}\u{2500} " } else { "\u{251C}\u{2500}\u{2500} " };
                    let child_prefix = if is_last { format!("{}    ", prefix) } else { format!("{}\u{2502}   ", prefix) };

                    let meta = match entry.metadata() {
                        Ok(m) => m,
                        Err(_) => continue,
                    };

                    if meta.is_dir() {
                        *dir_count += 1;
                        lines.push(format!("{}{}{}/", prefix, connector, name));
                        build_tree(&entry.path(), &child_prefix, depth + 1, max_depth, show_hidden, glob_re, lines, file_count, dir_count, total_size, max_entries);
                    } else if meta.is_file() {
                        // Apply glob filter on filename
                        if let Some(ref gre) = glob_re {
                            if !gre.is_match(&name) {
                                continue;
                            }
                        }
                        *file_count += 1;
                        *total_size += meta.len();
                        let size_str = if meta.len() < 1024 {
                            format!("{} B", meta.len())
                        } else if meta.len() < 1_048_576 {
                            format!("{:.1} KB", meta.len() as f64 / 1024.0)
                        } else {
                            format!("{:.1} MB", meta.len() as f64 / 1_048_576.0)
                        };
                        lines.push(format!("{}{}{} ({})", prefix, connector, name, size_str));
                    }
                }
            }

            build_tree(base_path, "", 0, max_depth, show_hidden, &glob_re, &mut tree_lines, &mut file_count, &mut dir_count, &mut total_size, MAX_ENTRIES);

            let total_human = if total_size < 1024 {
                format!("{} B", total_size)
            } else if total_size < 1_048_576 {
                format!("{:.1} KB", total_size as f64 / 1024.0)
            } else if total_size < 1_073_741_824 {
                format!("{:.1} MB", total_size as f64 / 1_048_576.0)
            } else {
                format!("{:.2} GB", total_size as f64 / 1_073_741_824.0)
            };

            Ok(json!({
                "success": true,
                "tree": tree_lines.join("\n"),
                "stats": {
                    "files": file_count,
                    "dirs": dir_count,
                    "total_size": total_size,
                    "total_size_human": total_human,
                },
                "truncated": tree_lines.len() >= MAX_ENTRIES,
            }))
        }

        "clipboard_read" => {
            let mut clipboard = arboard::Clipboard::new()
                .map_err(|e| format!("Failed to access clipboard: {}", e))?;
            let content = clipboard.get_text()
                .map_err(|e| format!("Failed to read clipboard: {}", e))?;

            Ok(json!({
                "success": true,
                "content": content,
                "length": content.len(),
            }))
        }

        "clipboard_write" => {
            let content = get_str(&args, "content")?;
            let mut clipboard = arboard::Clipboard::new()
                .map_err(|e| format!("Failed to access clipboard: {}", e))?;
            clipboard.set_text(&content)
                .map_err(|e| format!("Failed to write clipboard: {}", e))?;

            Ok(json!({
                "success": true,
                "message": format!("Copied {} characters to clipboard", content.len()),
                "length": content.len(),
            }))
        }

        // === APP CONTROL TOOLS ===

        "set_theme" => {
            let theme = get_str(&args, "theme")?;
            let valid_themes = ["light", "dark", "tokyo", "cyber"];
            if !valid_themes.contains(&theme.as_str()) {
                return Err(format!("Invalid theme '{}'. Valid themes: {}", theme, valid_themes.join(", ")));
            }
            app.emit("ai-set-theme", json!({ "theme": theme }))
                .map_err(|e| format!("Failed to emit theme event: {}", e))?;
            Ok(json!({
                "success": true,
                "theme": theme,
                "message": format!("Theme changed to '{}'", theme)
            }))
        }

        "app_info" => {
            let version = env!("CARGO_PKG_VERSION");
            let os = std::env::consts::OS;
            let arch = std::env::consts::ARCH;

            let has_prov = has_provider(&state).await;
            let has_ftp_conn = has_ftp(&app_state).await;

            let mut info = json!({
                "version": version,
                "platform": os,
                "arch": arch,
                "connected": has_prov || has_ftp_conn,
                "connection_type": if has_prov { "provider" } else if has_ftp_conn { "ftp" } else { "none" },
            });

            if let Some(ref local_path) = context_local_path {
                info["current_local_path"] = json!(local_path);
            }

            Ok(info)
        }

        "sync_control" => {
            let action = get_str(&args, "action")?;
            match action.as_str() {
                "status" => {
                    let running = crate::BACKGROUND_SYNC_RUNNING.load(std::sync::atomic::Ordering::SeqCst);
                    Ok(json!({
                        "success": true,
                        "sync_running": running,
                        "message": if running { "Background sync is running" } else { "Background sync is not running" }
                    }))
                }
                "start" => {
                    app.emit("ai-sync-control", json!({ "action": "start" }))
                        .map_err(|e| format!("Failed to emit sync control event: {}", e))?;
                    Ok(json!({
                        "success": true,
                        "message": "Background sync start requested"
                    }))
                }
                "stop" => {
                    app.emit("ai-sync-control", json!({ "action": "stop" }))
                        .map_err(|e| format!("Failed to emit sync control event: {}", e))?;
                    Ok(json!({
                        "success": true,
                        "message": "Background sync stop requested"
                    }))
                }
                _ => Err(format!("Invalid sync action '{}'. Use: start, stop, status", action))
            }
        }

        "vault_peek" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;

            let p = std::path::Path::new(&path);
            if !p.exists() {
                return Err(format!("Vault file not found: {}", path));
            }
            if !path.ends_with(".aerovault") {
                return Err("File is not an AeroVault container (.aerovault)".to_string());
            }

            let info = crate::aerovault_v2::vault_v2_peek(path).await?;
            Ok(info)
        }

        "shell_execute" => {
            let command = get_str(&args, "command")?;
            let working_dir = get_str_opt(&args, "working_dir");
            let timeout_secs = args.get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(30)
                .min(120);
            let result = shell_execute(command, working_dir, Some(timeout_secs)).await?;
            Ok(result)
        }

        _ => Err(format!("Tool not implemented: {}", tool_name)),
    }
}
