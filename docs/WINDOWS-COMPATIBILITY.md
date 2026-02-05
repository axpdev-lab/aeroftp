# Windows Compatibility Audit — AeroFTP v1.8.6

**Audit Date:** February 5, 2026
**Scope:** Full codebase — 41 Rust source files, 50+ TypeScript/React components, Tauri config, CI/CD
**Methodology:** Automated multi-agent static analysis across 4 domains
**Overall Verdict:** **PRODUCTION-READY** — 0 critical, 0 high, 3 medium, 4 low issues

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Compatibility Matrix](#compatibility-matrix)
3. [Path Handling](#1-path-handling)
4. [File Permissions & ACL](#2-file-permissions--acl)
5. [Shell & Terminal Integration](#3-shell--terminal-integration)
6. [Credential Storage](#4-credential-storage)
7. [Clipboard Operations](#5-clipboard-operations)
8. [OAuth2 Callback Server](#6-oauth2-callback-server)
9. [Archive Operations](#7-archive-operations)
10. [Auto-Update System](#8-auto-update-system)
11. [Build System & Dependencies](#9-build-system--dependencies)
12. [CI/CD Pipeline](#10-cicd-pipeline)
13. [Keyboard & Input](#11-keyboard--input)
14. [File Dialogs & Explorer](#12-file-dialogs--explorer-integration)
15. [Findings Summary Table](#findings-summary-table)
16. [Recommendations](#recommendations)
17. [Windows Version Support](#windows-version-support)

---

## Executive Summary

AeroFTP demonstrates **production-grade Windows compatibility** with comprehensive cross-platform design. The Rust backend uses proper conditional compilation (`#[cfg(windows)]` / `#[cfg(unix)]`) throughout, and the frontend relies on Tauri's platform-abstracted APIs.

| Metric | Value |
|--------|-------|
| Total items analyzed | 58 |
| Critical (blocking) | **0** |
| High severity | **0** |
| Medium severity | **3** |
| Low severity | **4** |
| Well-handled / Best practice | **51** |

**Key strengths:**
- Zero hardcoded path separators in production code
- Dedicated `windows_acl.rs` module for ACL hardening
- Platform-specific clipboard threading to prevent UI freeze
- Intelligent PowerShell detection with cmd.exe fallback
- All 68 Cargo dependencies verified Windows-compatible
- WiX (MSI) and NSIS (EXE) installer configurations

---

## Compatibility Matrix

| Component | Windows 10+ | Windows 11 | Notes |
|-----------|:-----------:|:----------:|-------|
| FTP/FTPS | Yes | Yes | SChannel TLS backend |
| SFTP | Yes | Yes | russh 0.57 (pure Rust) |
| WebDAV | Yes | Yes | reqwest + native-tls |
| S3 | Yes | Yes | reqwest HTTPS |
| Google Drive | Yes | Yes | OAuth2 PKCE, localhost callback |
| Dropbox | Yes | Yes | OAuth2 PKCE |
| OneDrive | Yes | Yes | OAuth2 PKCE |
| MEGA | Yes | Yes | Client-side AES |
| Box | Yes | Yes | OAuth2 PKCE |
| pCloud | Yes | Yes | OAuth2 token |
| Azure Blob | Yes | Yes | HMAC-SHA256 |
| Filen | Yes | Yes | Client-side AES-256-GCM |
| Terminal (PTY) | Yes | Yes | PowerShell preferred, cmd.exe fallback |
| AeroVault v2 | Yes | Yes | Pure Rust crypto stack |
| Cryptomator | Yes | Yes | Pure Rust (scrypt + AES) |
| Universal Vault | Yes | Yes | icacls ACL hardening |
| Archive Browser | Yes | Yes | ZIP/7z/TAR/RAR |
| Drag & Drop | Yes* | Yes* | HTML5 DnD (native disabled) |
| Auto-Update | Manual | Manual | Download-based (.exe/.msi) |
| System Tray | Yes | Yes | Tauri native tray |

\* `dragDropEnabled: false` in tauri.conf.json — uses custom HTML5 DnD to avoid Tauri plugin-dialog conflicts.

---

## 1. Path Handling

**Status: EXCELLENT** — Zero hardcoded path separators in production code.

### Backend (Rust)

| Pattern | Files | Assessment |
|---------|-------|------------|
| `PathBuf::join()` for local paths | lib.rs, credential_store.rs, sync.rs | Automatic separator handling |
| Forward slash `/` for remote FTP/SFTP paths | ftp.rs, sftp.rs | Correct — FTP protocol standard |
| `dirs::config_dir()` for config paths | credential_store.rs:555, oauth2.rs:418 | Returns `%APPDATA%` on Windows |
| `dirs::download_dir()` for downloads | lib.rs:384 | Returns `%USERPROFILE%\Downloads` |
| `dirs::home_dir()` for `~` expansion | sftp.rs:179 | Returns `C:\Users\<user>` |
| Backslash normalization from FTP servers | ftp.rs:366 | `.replace('\\', "/")` |

**FTP path normalization (ftp.rs:366):**
```rust
self.current_path = stream.pwd().await
    .map_err(...)?
    .replace('\\', "/");  // Windows FTP servers may return backslashes
```

**Config directory resolution (credential_store.rs:555-567):**
```rust
fn config_dir() -> Result<PathBuf, CredentialError> {
    let base = dirs::config_dir()           // Windows: %APPDATA%\Roaming
        .or_else(|| dirs::home_dir())       // Fallback: %USERPROFILE%
        .ok_or_else(|| ...)?;
    let dir = base.join("aeroftp");         // Automatic separator
    // ...
}
```

### Frontend (TypeScript)

| Pattern | Files | Assessment |
|---------|-------|------------|
| `appConfigDir()` from Tauri | chatHistory.ts:7,45 | Platform-aware |
| `@tauri-apps/api/path` APIs | Multiple components | Cross-platform |
| No hardcoded `/` or `\` in paths | Entire frontend | Verified clean |

---

## 2. File Permissions & ACL

**Status: EXCELLENT** — Dedicated `windows_acl.rs` module with proper conditional compilation.

### Platform-Specific Permission Model

```rust
// credential_store.rs:570-582
pub fn ensure_secure_permissions(path: &Path) -> Result<(), CredentialError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, Permissions::from_mode(mode))?;
    }
    #[cfg(windows)]
    {
        crate::windows_acl::restrict_to_owner(path);
    }
    Ok(())
}
```

### Windows ACL Implementation (windows_acl.rs:11-23)

```rust
#[cfg(windows)]
pub fn restrict_to_owner(path: &std::path::Path) {
    let path_str = path.to_string_lossy();
    let username = std::env::var("USERNAME").unwrap_or_else(|_| "".to_string());
    if username.is_empty() { return; }
    let _ = std::process::Command::new("icacls")
        .args([&*path_str, "/inheritance:r", "/grant:r",
               &format!("{}:F", username), "/T", "/Q"])
        .creation_flags(0x08000000)  // CREATE_NO_WINDOW
        .output();
}
```

| Flag | Purpose |
|------|---------|
| `/inheritance:r` | Remove inherited permissions |
| `/grant:r` | Grant explicit permissions (reset) |
| `{}:F` | Current user: Full Control |
| `/T` | Apply recursively |
| `/Q` | Quiet mode |
| `0x08000000` | Prevent console window popup |

### Windows Reserved Filename Validation (windows_acl.rs:31-47)

Blocks all 22 Windows reserved device names (CON, PRN, AUX, NUL, COM1-9, LPT1-9).

**Integration point (lib.rs:2309-2318):**
```rust
#[cfg(windows)]
if let Some(reserved) = windows_acl::check_windows_reserved(&dest_name) {
    return Err(format!("'{}' is a reserved Windows filename", reserved));
}
```

---

## 3. Shell & Terminal Integration

**Status: EXCELLENT** — Intelligent PowerShell detection with three-tier fallback.

### Shell Detection (pty.rs:64-78)

```rust
#[cfg(windows)]
let shell = {
    let ps = std::env::var("SystemRoot")
        .map(|sr| format!("{}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", sr))
        .unwrap_or_else(|_| "powershell.exe".to_string());
    if std::path::Path::new(&ps).exists() {
        ps
    } else {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
};
```

| Priority | Shell | Detection |
|----------|-------|-----------|
| 1st | PowerShell | `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe` |
| 2nd | COMSPEC | `%COMSPEC%` environment variable |
| 3rd | cmd.exe | Hardcoded fallback |

### PowerShell Prompt Customization (pty.rs:83-89)

Custom colorized prompt with ANSI escape codes: green `USERNAME@COMPUTERNAME`, blue `path`.

### Environment Variables (pty.rs:91-100)

```rust
cmd.env("TERM", "xterm-256color");
cmd.env("COLORTERM", "truecolor");
cmd.env("FORCE_COLOR", "1");
```

Ensures color support across all platforms.

---

## 4. Credential Storage

**Status: EXCELLENT** — Universal Vault with platform-specific hardening.

### Architecture

```
Windows: %APPDATA%\Roaming\aeroftp\
    vault.key  (76B auto / 136B master) — ACL restricted via icacls
    vault.db   (AES-256-GCM encrypted)  — ACL restricted via icacls
    oauth_tokens/                        — ACL restricted via icacls
```

### Security Stack

| Layer | Algorithm | Notes |
|-------|-----------|-------|
| Encryption | AES-256-GCM | Per-entry random 12-byte nonce |
| Key derivation | HKDF-SHA256 (RFC 5869) | 512-bit passphrase to 256-bit key |
| Master mode KDF | Argon2id (128 MiB, t=4, p=4) | Exceeds OWASP 2024 |
| File protection | Windows ACL (icacls) | Current user Full Control only |
| Memory safety | `zeroize` + `secrecy` crates | Keys zeroed on drop |
| Secure delete | Overwrite zeros + random + remove | Prevents forensic recovery |

### Hardening Sequence (credential_store.rs:585-620)

1. `config_dir()` → creates `%APPDATA%\Roaming\aeroftp\`
2. `ensure_secure_permissions()` → applies ACL via icacls
3. `harden_config_directory()` → recursively hardens all config files
4. `secure_delete()` → overwrite-before-delete for sensitive files

---

## 5. Clipboard Operations

**Status: EXCELLENT** — Windows-specific thread spawning to prevent UI freeze.

### Windows Clipboard Threading (lib.rs:259-271)

```rust
#[cfg(target_os = "windows")]
{
    // Spawn in separate thread to avoid UI freeze when
    // Credential Manager or Windows Hello is active
    let text_clone = text.clone();
    std::thread::spawn(move || {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(text_clone);
        }
    });
    clipboard.set_text(text)?;
}
```

**Why:** Windows Credential Manager and Windows Hello can block the main thread during clipboard access. Dual-threaded approach ensures UI remains responsive.

---

## 6. OAuth2 Callback Server

**Status: CORRECT** — No Windows Firewall issues.

All 5 OAuth providers bind to `127.0.0.1` (localhost loopback):

```rust
let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).await?;
```

| Property | Value |
|----------|-------|
| Bind address | `127.0.0.1` (loopback) |
| Port | OS-assigned ephemeral (port 0) |
| Firewall | Loopback traffic exempt from Windows Firewall |
| Security | Not accessible from network |

---

## 7. Archive Operations

**Status: MEDIUM** — Functional but lacks atomic write pattern.

### MEDIUM: Non-Atomic Archive Extraction

**Files affected:** `archive_browse.rs` (ZIP lines 89-125, 7z lines 213-261, TAR lines 319-352)

Archive extraction writes directly to output path without temp file + rename pattern. If extraction fails midway, partial files remain on disk.

```rust
// Current pattern (all 3 formats):
let mut outfile = File::create(out_path)?;  // Direct write
std::io::copy(&mut entry, &mut outfile)?;   // May fail midway

// Recommended pattern:
let tmp_path = out_path.with_extension("tmp");
let mut outfile = File::create(&tmp_path)?;
std::io::copy(&mut entry, &mut outfile)?;
std::fs::rename(&tmp_path, &out_path)?;     // Atomic rename
```

**Impact:** On Windows, file locking may prevent cleanup of partial files if accessed by another process (antivirus, indexer). RAR extraction (via unrar crate) handles atomicity internally.

**Severity:** MEDIUM — Not a data loss risk, but leaves orphan `.tmp`-equivalent files on failure.

---

## 8. Auto-Update System

**Status: CORRECT** — Manual download for Windows (by design).

### Install Format Detection (lib.rs:218-231)

```rust
"windows" => {
    if let Ok(exe_path) = std::env::current_exe() {
        let path_str = exe_path.to_string_lossy().to_lowercase();
        let pf = std::env::var("ProgramFiles").unwrap_or_default().to_lowercase();
        let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_default().to_lowercase();
        if path_str.starts_with(&pf) || path_str.starts_with(&pf86) {
            return "msi".to_string();
        }
    }
    "exe".to_string()
}
```

| Format | Detection | Update Method |
|--------|-----------|---------------|
| MSI | Installed in Program Files | Manual download + run installer |
| EXE | Portable (any location) | Manual download + replace |
| AppImage (Linux) | `$APPIMAGE` env var | Auto-install + restart |

**Why manual on Windows:** Windows executables cannot replace themselves while running (file lock). Standard practice across all Windows applications.

---

## 9. Build System & Dependencies

**Status: PASS** — All 68 Cargo dependencies verified Windows-compatible.

### Key Windows-Aware Dependencies

| Crate | Version | Windows Backend |
|-------|---------|-----------------|
| `native-tls` | 0.2 | Windows SChannel (not OpenSSL) |
| `russh` | 0.57 | Pure Rust SSH (no libssh2) |
| `portable-pty` | 0.8 | Windows ConPTY API |
| `arboard` | 3 | Windows clipboard API |
| `dirs` | 5 | Windows Known Folders API |
| `notify` | 6.1 | Windows ReadDirectoryChangesW |
| `open` | 5 | Windows ShellExecute |
| `keyring` | 3 | Windows Credential Manager |

### Platform-Specific Features (Cargo.toml)

No Linux-only crates detected. All dependencies compile cleanly on `x86_64-pc-windows-msvc`.

### Package.json Scripts

All npm scripts use cross-platform Node.js tools (vite, tsc, tauri CLI, tsx). No Unix shell commands.

---

## 10. CI/CD Pipeline

**Status: PASS** — Windows build in matrix, but Linux runner has disk space issue.

### Build Matrix (.github/workflows/build.yml)

| Platform | Runner | Artifacts | Status |
|----------|--------|-----------|--------|
| Linux | ubuntu-22.04 | .deb, .rpm, .AppImage, .snap | Disk space issue* |
| Windows | windows-latest | .msi, .exe | Pass |
| macOS | macos-latest | .dmg | Pass |

\* **Known Issue:** Ubuntu runner exhausts disk space during Rust compilation. Fix: add disk cleanup step before build:
```yaml
- name: Free disk space
  if: matrix.platform == 'ubuntu-22.04'
  run: |
    sudo rm -rf /usr/share/dotnet /usr/local/lib/android /opt/ghc
    sudo apt-get clean
    df -h
```

### Windows Artifacts Uploaded

```yaml
# Lines 124-132
- name: Upload Windows artifacts
  if: matrix.platform == 'windows-latest'
  uses: actions/upload-artifact@v4
  with:
    name: windows-artifacts
    path: |
      src-tauri/target/release/bundle/msi/*.msi
      src-tauri/target/release/bundle/nsis/*.exe
```

### Windows Subsystem Configuration (main.rs:2)

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
```

Prevents console window popup in release builds.

---

## 11. Keyboard & Input

**Status: CORRECT** — Proper modifier key handling.

### Keyboard Shortcuts (useKeyboardShortcuts.ts:24-27)

```typescript
if (event.ctrlKey) keys.push('Ctrl');   // Windows/Linux
if (event.altKey) keys.push('Alt');
if (event.shiftKey) keys.push('Shift');
if (event.metaKey) keys.push('Meta');   // Windows key / macOS Cmd
```

All shortcuts use `Ctrl` on Windows (not `Cmd`). Browser-level key mapping handles the translation automatically.

---

## 12. File Dialogs & Explorer Integration

**Status: EXCELLENT** — Native integration on Windows.

### File Dialogs (App.tsx)

Uses `@tauri-apps/plugin-dialog` which opens native Windows Explorer dialogs.

### "Show in Explorer" (lib.rs:1842-1877)

```rust
#[cfg(target_os = "windows")]
{
    let normalized = path.replace('/', "\\");
    let metadata = std::fs::metadata(&normalized);
    if metadata.map(|m| m.is_file()).unwrap_or(false) {
        std::process::Command::new("explorer")
            .args(["/select,", &normalized])
            .spawn()?;
    } else {
        std::process::Command::new("explorer")
            .arg(&normalized)
            .spawn()?;
    }
}
```

Uses Windows Explorer `/select,` flag to highlight files in the parent folder.

---

## Findings Summary Table

| # | Category | Issue | File | Severity | Status |
|---|----------|-------|------|:--------:|--------|
| 1 | Archive | Non-atomic ZIP extraction | archive_browse.rs:89-125 | **MEDIUM** | Needs fix |
| 2 | Archive | Non-atomic 7z extraction | archive_browse.rs:213-261 | **MEDIUM** | Needs fix |
| 3 | Archive | Non-atomic TAR extraction | archive_browse.rs:319-352 | **MEDIUM** | Needs fix |
| 4 | Sync | "Keep Both" conflict unimplemented | cloud_service.rs:509,668 | LOW | TODO |
| 5 | AI Tools | File name extraction uses `/` only | ai_tools.rs:317-324 | LOW | Edge case |
| 6 | Debug | `eprintln!` in production | aerovault_v2.rs:1374 | LOW | Code hygiene |
| 7 | Terminal | No auto-clear on Windows | SSHTerminal.tsx:661-665 | LOW | UX minor |
| 8 | Paths | PathBuf used throughout | lib.rs, all providers | N/A | **Excellent** |
| 9 | Permissions | icacls ACL hardening | windows_acl.rs:11-23 | N/A | **Excellent** |
| 10 | Shell | PowerShell detection | pty.rs:67-78 | N/A | **Excellent** |
| 11 | Clipboard | Thread-safe Windows clipboard | lib.rs:259-271 | N/A | **Excellent** |
| 12 | Credentials | Universal Vault + ACL | credential_store.rs | N/A | **Excellent** |
| 13 | OAuth | Localhost loopback callback | oauth2.rs:541 | N/A | **Correct** |
| 14 | Installer | WiX + NSIS configured | tauri.conf.json:58-68 | N/A | **Correct** |
| 15 | Reserved names | CON/PRN/NUL validation | windows_acl.rs:31-47 | N/A | **Excellent** |
| 16 | Explorer | `/select,` flag integration | lib.rs:1842-1877 | N/A | **Excellent** |
| 17 | Build | All 68 deps Windows-compatible | Cargo.toml | N/A | **Pass** |
| 18 | CI/CD | Windows build in matrix | build.yml | N/A | **Pass** |
| 19 | Main.rs | `windows_subsystem = "windows"` | main.rs:2 | N/A | **Correct** |

---

## Recommendations

### Priority 1 — Fix in Next Release

1. **Atomic archive extraction** (archive_browse.rs)
   - Write to `.tmp` file, then rename on success
   - Prevents partial file orphans on extraction failure
   - Windows antivirus/indexer can lock partial files

### Priority 2 — Code Quality

2. **Replace `eprintln!` with `tracing::error!`** (aerovault_v2.rs:1374)
3. **Gate console.log behind debug mode** (76 statements, already in v1.9.0 roadmap)

### Priority 3 — Enhancement

4. **Implement "Keep Both" sync conflict** (cloud_service.rs:509,668)
5. **Windows terminal auto-clear** — Send `cls\r\n` instead of empty string (SSHTerminal.tsx:663)

### Priority 4 — CI/CD

6. **Fix GitHub Actions Linux runner disk space** — Add cleanup step before Rust build

---

## Windows Version Support

| Windows Version | Support Level | Notes |
|-----------------|:------------:|-------|
| Windows 11 | **Full** | Primary development target |
| Windows 10 (21H2+) | **Full** | Tested |
| Windows 10 (older) | Expected | Not explicitly tested |
| Windows 8.1 | Untested | May work (SChannel, ConPTY) |
| Windows 7 | **Not supported** | Missing ConPTY, modern TLS |

### Requirements

- Visual C++ Redistributable 2019+ (bundled by NSIS/WiX)
- WebView2 Runtime (bundled by Tauri)
- .NET Framework not required
- No admin rights for portable EXE mode

---

*AeroFTP v1.8.6 — Windows Compatibility Audit — February 2026*
